use clap::App;
use hbb_common::{
    allow_err, anyhow::{Context, Result}, get_version_number, log, tokio, ResultType
};
use ini::Ini;
use sodiumoxide::crypto::sign;
use std::{
    io::prelude::*,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Instant, SystemTime},
};

pub fn parse_bind_address(value: &str) -> Result<Option<IpAddr>> {
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse()
            .with_context(|| format!("Invalid bind address: {value}"))
            .map(Some)
    }
}

pub async fn listen_tcp(
    bind_addr: Option<IpAddr>,
    port: u16,
) -> ResultType<hbb_common::tokio::net::TcpListener> {
    if let Some(bind_addr) = bind_addr {
        hbb_common::tcp::new_listener(SocketAddr::new(bind_addr, port), true).await
    } else {
        hbb_common::tcp::listen_any(port).await
    }
}

pub fn console_addr(bind_addr: Option<IpAddr>, port: u16) -> Option<SocketAddr> {
    let bind_addr = bind_addr?;
    if bind_addr.is_unspecified() || bind_addr == IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return None;
    }
    Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
}

// The runtime console (check_cmd) is reached via 127.0.0.1, so when the bind
// address does not already accept connections to 127.0.0.1 (it is neither the
// any-address nor 127.0.0.1 itself), the console gets a dedicated listener
// there; it is never bound to the external bind address.
pub async fn listen_console(
    bind_addr: Option<IpAddr>,
    port: u16,
) -> ResultType<Option<hbb_common::tokio::net::TcpListener>> {
    match console_addr(bind_addr, port) {
        Some(addr) => {
            let listener = hbb_common::tcp::new_listener(addr, true).await?;
            log::info!("Listening on tcp {} for the console", addr);
            Ok(Some(listener))
        }
        None => Ok(None),
    }
}

pub async fn accept_or_pending(
    listener: Option<&hbb_common::tokio::net::TcpListener>,
) -> std::io::Result<(hbb_common::tokio::net::TcpStream, SocketAddr)> {
    match listener {
        Some(listener) => listener.accept().await,
        None => std::future::pending().await,
    }
}

#[allow(dead_code)]
pub(crate) fn get_expired_time() -> Instant {
    let now = Instant::now();
    now.checked_sub(std::time::Duration::from_secs(3600))
        .unwrap_or(now)
}

#[allow(dead_code)]
pub(crate) fn test_if_valid_server(host: &str, name: &str) -> ResultType<SocketAddr> {
    use std::net::ToSocketAddrs;
    let res = if host.contains(':') {
        host.to_socket_addrs()?.next().context("")
    } else {
        format!("{}:{}", host, 0)
            .to_socket_addrs()?
            .next()
            .context("")
    };
    if res.is_err() {
        log::error!("Invalid {} {}: {:?}", name, host, res);
    }
    res
}

#[allow(dead_code)]
pub(crate) fn get_servers(s: &str, tag: &str) -> Vec<String> {
    let servers: Vec<String> = s
        .split(',')
        .filter(|x| !x.is_empty() && test_if_valid_server(x, tag).is_ok())
        .map(|x| x.to_owned())
        .collect();
    log::info!("{}={:?}", tag, servers);
    servers
}

#[allow(dead_code)]
#[inline]
fn arg_name(name: &str) -> String {
    name.to_uppercase().replace('_', "-")
}

#[allow(dead_code)]
#[inline]
pub fn set_arg(name: &str, value: &str) {
    std::env::set_var(arg_name(name), value);
}

#[allow(dead_code)]
pub fn init_args(args: &str, name: &str, about: &str) {
    let matches = App::new(name)
        .version(crate::version::VERSION)
        .author("Purslane Ltd. <info@rustdesk.com>")
        .about(about)
        .args_from_usage(args)
        .get_matches();
    if let Ok(v) = Ini::load_from_file(".env") {
        if let Some(section) = v.section(None::<String>) {
            section
                .iter()
                .for_each(|(k, v)| set_arg(k, v));
        }
    }
    if let Some(config) = matches.value_of("config") {
        if let Ok(v) = Ini::load_from_file(config) {
            if let Some(section) = v.section(None::<String>) {
                section
                    .iter()
                    .for_each(|(k, v)| set_arg(k, v));
            }
        }
    }
    for (k, v) in matches.args {
        if let Some(v) = v.vals.first() {
            set_arg(k, &v.to_string_lossy());
        }
    }
}

#[allow(dead_code)]
pub fn get_arg_opt(name: &str) -> Option<String> {
    let dashed = arg_name(name);
    let underscored = dashed.replace('-', "_");
    let lower_dashed = dashed.to_lowercase();
    let lower_underscored = underscored.to_lowercase();
    for alias in [&dashed, &underscored, &lower_dashed, &lower_underscored] {
        if let Ok(value) = std::env::var(alias) {
            return Some(value);
        }
    }
    let mut aliases = std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.into_string().ok()?;
            if arg_name(&key) == dashed {
                Some((key, value.into_string().ok()?))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    aliases.sort_by(|a, b| a.0.cmp(&b.0));
    aliases.into_iter().next().map(|(_, value)| value)
}

#[allow(dead_code)]
#[inline]
pub fn get_arg(name: &str) -> String {
    get_arg_or(name, "".to_owned())
}

#[allow(dead_code)]
#[inline]
pub fn get_arg_or(name: &str, default: String) -> String {
    get_arg_opt(name).unwrap_or(default)
}

#[allow(dead_code)]
#[inline]
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|x| x.as_secs())
        .unwrap_or_default()
}

#[cfg(unix)]
const PRIVATE_KEY_FILE_MODE: u32 = 0o600;

#[cfg(not(unix))]
fn create_private_key_file(path: &str) -> ResultType<std::fs::File> {
    Ok(std::fs::File::create(path)?)
}

#[cfg(unix)]
fn create_private_key_file(path: &str) -> ResultType<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut path = std::path::PathBuf::from(path);
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(PRIVATE_KEY_FILE_MODE);
    loop {
        match options.open(&path) {
            Ok(file) => {
                if let Err(err) = set_private_key_permissions(&file, true) {
                    drop(file);
                    std::fs::remove_file(&path).with_context(|| {
                        format!("Failed to remove {} after {err:#}", path.display())
                    })?;
                    return Err(err);
                }
                return Ok(file);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                // Let the OS reject symlink loops before following a dangling link.
                match std::fs::metadata(&path) {
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                    Ok(_) => return Err(err.into()),
                }
                let target = std::fs::read_link(&path)?;
                path.pop();
                path.push(target);
            }
            Err(err) => return Err(err.into()),
        }
    }
}

#[cfg(unix)]
fn set_private_key_permissions(file: &std::fs::File, newly_created: bool) -> ResultType<()> {
    use std::os::unix::fs::PermissionsExt;

    const PERMISSION_BITS: u32 = 0o7777;
    let mode = file.metadata()?.permissions().mode() & PERMISSION_BITS;
    if !newly_created && mode & !PRIVATE_KEY_FILE_MODE == 0 {
        return Ok(());
    }
    hbb_common::anyhow::ensure!(
        !newly_created || mode & !PRIVATE_KEY_FILE_MODE == 0,
        "Unsafe initial private key permissions: {mode:04o}"
    );
    if mode != PRIVATE_KEY_FILE_MODE {
        file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_KEY_FILE_MODE))
            .context("Failed to set private key permissions to 0600")?;
        let mode = file.metadata()?.permissions().mode() & PERMISSION_BITS;
        hbb_common::anyhow::ensure!(
            mode == PRIVATE_KEY_FILE_MODE,
            "Private key permissions are {mode:04o}, expected 0600"
        );
    }
    Ok(())
}

fn write_public_key(path: &str, pk: &str, required: bool) -> ResultType<()> {
    match std::fs::read_to_string(path) {
        Ok(contents) if contents.trim() == pk => return Ok(()),
        Ok(_) => {
            let err = hbb_common::anyhow::anyhow!(
                "Public key in {path} does not match private key in id_ed25519"
            );
            log::error!("{err}");
            return Err(err);
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).context("Failed to read public key file"),
    }
    let mut file = match std::fs::File::create(path) {
        Ok(file) => file,
        // Existing private keys may be provisioned in a read-only directory.
        Err(err) if !required => {
            log::warn!("Failed to create {path}: {err}; using the existing private key");
            return Ok(());
        }
        Err(err) => return Err(err).context("Failed to create public key file"),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        const OWNER_READ_WRITE: u32 = 0o600;
        let mode = file.metadata()?.permissions().mode();
        if mode & OWNER_READ_WRITE != OWNER_READ_WRITE {
            file.set_permissions(std::fs::Permissions::from_mode(mode | OWNER_READ_WRITE))
                .context("Failed to set public key owner permissions")?;
        }
    }
    file.write_all(pk.as_bytes())
        .context("Failed to write public key")
}

pub fn gen_sk(wait: u64) -> ResultType<(String, Option<sign::SecretKey>)> {
    let sk_file = "id_ed25519";
    let pub_file = format!("{sk_file}.pub");
    if wait > 0 && !std::path::Path::new(sk_file).exists() {
        std::thread::sleep(std::time::Duration::from_millis(wait));
    }
    match std::fs::File::open(sk_file) {
        Ok(mut file) => {
            #[cfg(unix)]
            set_private_key_permissions(&file, false)?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .context("Failed to read private key")?;
            let sk = base64::decode(contents.trim()).context("Malformed private key")?;
            hbb_common::anyhow::ensure!(sk.len() == sign::SECRETKEYBYTES, "Malformed private key");
            let mut tmp = [0u8; sign::SECRETKEYBYTES];
            tmp[..].copy_from_slice(&sk);
            let pk = base64::encode(&tmp[sign::SECRETKEYBYTES / 2..]);
            write_public_key(&pub_file, &pk, false)?;
            log::info!("Private key comes from {}", sk_file);
            return Ok((pk, Some(sign::SecretKey(tmp))));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).context("Failed to open private key"),
    }
    let gen_func = || {
        let (tmp, sk) = sign::gen_keypair();
        (base64::encode(tmp), sk)
    };
    let (mut pk, mut sk) = gen_func();
    for _ in 0..300 {
        if !pk.contains('/') && !pk.contains(':') {
            break;
        }
        (pk, sk) = gen_func();
    }
    let mut f = create_private_key_file(sk_file).context("Failed to create private key file")?;
    f.write_all(base64::encode(&sk).as_bytes())
        .context("Failed to write private key")?;
    write_public_key(&pub_file, &pk, true)?;
    log::info!("Private/public key written to {}/{}", sk_file, pub_file);
    log::debug!("Public key: {}", pk);
    Ok((pk, Some(sk)))
}

#[cfg(unix)]
pub async fn listen_signal() -> Result<()> {
    use hbb_common::tokio;
    use hbb_common::tokio::signal::unix::{signal, SignalKind};

    tokio::spawn(async {
        let mut s = signal(SignalKind::terminate())?;
        let terminate = s.recv();
        let mut s = signal(SignalKind::interrupt())?;
        let interrupt = s.recv();
        let mut s = signal(SignalKind::quit())?;
        let quit = s.recv();

        tokio::select! {
            _ = terminate => {
                log::info!("signal terminate");
            }
            _ = interrupt => {
                log::info!("signal interrupt");
            }
            _ = quit => {
                log::info!("signal quit");
            }
        }
        Ok(())
    })
    .await?
}

#[cfg(not(unix))]
pub async fn listen_signal() -> Result<()> {
    let () = std::future::pending().await;
    unreachable!();
}


pub fn check_software_update() {
    const ONE_DAY_IN_SECONDS: u64 = 60 * 60 * 24;
    std::thread::spawn(move || loop {
        std::thread::spawn(move || allow_err!(check_software_update_()));
        std::thread::sleep(std::time::Duration::from_secs(ONE_DAY_IN_SECONDS));
    });
}

#[tokio::main(flavor = "current_thread")]
async fn check_software_update_() -> hbb_common::ResultType<()> {
    let (request, url) = hbb_common::version_check_request(hbb_common::VER_TYPE_RUSTDESK_SERVER.to_string());
    let latest_release_response = reqwest::Client::builder().build()?
        .post(url)
        .json(&request)
        .send()
        .await?;

    let bytes = latest_release_response.bytes().await?;
    let resp: hbb_common::VersionCheckResponse = serde_json::from_slice(&bytes)?;
    let response_url = resp.url;
    let latest_release_version = response_url.rsplit('/').next().unwrap_or_default();
    if get_version_number(&latest_release_version) > get_version_number(crate::version::VERSION) {
       log::info!("new version is available: {}", latest_release_version);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn argument_names_ignore_case_and_separator() {
        let aliases = [
            "RUSTDESK-CONFIG-ALIAS-TEST",
            "RUSTDESK_CONFIG_ALIAS_TEST",
            "rustdesk-config-alias-test",
            "rustdesk_config_alias_test",
            "RustDesk_Config-Alias_Test",
        ];
        for alias in aliases {
            std::env::remove_var(alias);
        }
        for alias in aliases {
            std::env::set_var(alias, alias);
            assert_eq!(get_arg("RUSTDESK_CONFIG_ALIAS_TEST"), alias);
            std::env::remove_var(alias);
        }
        set_arg("rustdesk_config_alias_test", "normalized");
        assert_eq!(
            std::env::var("RUSTDESK-CONFIG-ALIAS-TEST").unwrap(),
            "normalized"
        );
        std::env::set_var("RUSTDESK_CONFIG_ALIAS_TEST", "inherited");
        set_arg("rustdesk-config-alias-test", "higher-priority");
        assert_eq!(get_arg("rustdesk_config_alias_test"), "higher-priority");
        std::env::remove_var("RUSTDESK-CONFIG-ALIAS-TEST");
        std::env::remove_var("RUSTDESK_CONFIG_ALIAS_TEST");
    }

    #[test]
    fn parses_bind_address() {
        assert_eq!(parse_bind_address("").unwrap(), None);
        assert_eq!(
            parse_bind_address("127.0.0.1").unwrap(),
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
        assert_eq!(
            parse_bind_address("::1").unwrap(),
            Some(IpAddr::V6(Ipv6Addr::LOCALHOST))
        );
        assert!(parse_bind_address("not-an-ip").is_err());
    }

    #[hbb_common::tokio::test]
    async fn tcp_listener_uses_bind_address() {
        let bind_addr = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let listener = listen_tcp(Some(bind_addr), 0).await.unwrap();
        assert_eq!(listener.local_addr().unwrap().ip(), bind_addr);
    }

    #[test]
    fn console_addr_only_when_bind_does_not_cover_ipv4_localhost() {
        for bind_addr in [
            None,
            Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            Some(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ] {
            assert_eq!(console_addr(bind_addr, 21117), None);
        }
        for bind_addr in [
            Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
            Some("2001:db8::1".parse().unwrap()),
            Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        ] {
            assert_eq!(
                console_addr(bind_addr, 21117),
                Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 21117))
            );
        }
    }

    #[hbb_common::tokio::test]
    async fn console_listener_binds_ipv4_localhost() {
        let listener = listen_console(Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))), 0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            listener.local_addr().unwrap().ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert!(listen_console(None, 0).await.unwrap().is_none());
        assert!(listen_console(Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), 0)
            .await
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn private_key_file_permissions() {
        use std::process::Command;

        const CHILD_ENV: &str = "RUSTDESK_PRIVATE_KEY_TEST_CHILD";
        const CHILD_COMPLETED: &str = "private_key_test_child_completed";
        const TEST_NAME: &str = "common::tests::private_key_file_permissions";
        if std::env::var_os(CHILD_ENV).is_some() {
            const TEST_UMASK: hbb_common::libc::mode_t = 0o022;
            // The child process isolates the umask from other tests.
            unsafe { hbb_common::libc::umask(TEST_UMASK) };
            assert_private_key_file_permissions();
            println!("{CHILD_COMPLETED}");
            return;
        }

        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_ENV, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(CHILD_COMPLETED),
            "key-file test failed ({}):\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    fn assert_public_key_recovery() -> String {
        let sk_file = "id_ed25519";
        let pub_file = "id_ed25519.pub";
        std::fs::create_dir(pub_file).unwrap();
        assert!(gen_sk(0).is_err());
        let private_key = std::fs::read(sk_file).unwrap();
        std::fs::remove_dir(pub_file).unwrap();
        let public_key = gen_sk(0).unwrap().0;
        assert_eq!(std::fs::read_to_string(pub_file).unwrap(), public_key);
        let mismatched_key = base64::encode(sign::gen_keypair().0);
        std::fs::write(pub_file, &mismatched_key).unwrap();
        assert!(gen_sk(0).is_err());
        assert!(std::fs::read(sk_file).unwrap() == private_key);
        assert_eq!(std::fs::read_to_string(pub_file).unwrap(), mismatched_key);
        std::fs::write(pub_file, &public_key).unwrap();
        let permissions = std::fs::metadata(pub_file).unwrap().permissions();
        let mut read_only = permissions.clone();
        read_only.set_readonly(true);
        std::fs::set_permissions(pub_file, read_only).unwrap();
        assert_eq!(gen_sk(0).unwrap().0, public_key);
        std::fs::set_permissions(pub_file, permissions).unwrap();
        public_key
    }

    #[cfg(unix)]
    fn assert_private_key_file_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        const PERMISSION_BITS: u32 = 0o777;
        const NEW_FILE_MODE: u32 = 0o600;
        const EXISTING_MODE: u32 = 0o644;
        const READ_ONLY_MODE: u32 = 0o400;
        const RESTRICTIVE_UMASK: hbb_common::libc::mode_t = 0o777;
        let directory =
            std::env::temp_dir().join(format!("rustdesk-private-key-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::env::set_current_dir(&directory).unwrap();
        let path = std::path::Path::new("id_ed25519");
        let mode = || std::fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS;

        let public_key = assert_public_key_recovery();
        assert_eq!(mode(), NEW_FILE_MODE);
        let contents = std::fs::read(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(EXISTING_MODE)).unwrap();
        let file = std::fs::File::open(path).unwrap();
        assert!(set_private_key_permissions(&file, true).is_err());
        assert_eq!(mode(), EXISTING_MODE);
        gen_sk(0).unwrap();
        assert_eq!(mode(), NEW_FILE_MODE);
        assert_eq!(std::fs::read(path).unwrap(), contents);
        assert!(create_private_key_file("id_ed25519").is_err());
        assert_eq!(std::fs::read(path).unwrap(), contents);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(READ_ONLY_MODE)).unwrap();
        assert_eq!(gen_sk(0).unwrap().0, public_key);
        assert_eq!(mode(), READ_ONLY_MODE);

        std::fs::remove_file(path).unwrap();
        symlink("missing/key", path).unwrap();
        assert!(gen_sk(0).is_err());
        assert!(std::fs::read_to_string("id_ed25519.pub").unwrap() == public_key);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file("id_ed25519.pub").unwrap();
        symlink("key-alias", path).unwrap();
        symlink("key-target", "key-alias").unwrap();
        gen_sk(0).unwrap();
        assert_eq!(mode(), NEW_FILE_MODE);
        assert!(!std::fs::read("key-target").unwrap().is_empty());

        std::fs::remove_file(path).unwrap();
        std::fs::remove_file("id_ed25519.pub").unwrap();
        unsafe { hbb_common::libc::umask(RESTRICTIVE_UMASK) };
        gen_sk(0).unwrap();
        assert_eq!(mode(), NEW_FILE_MODE);
        let contents = std::fs::read(path).unwrap();
        gen_sk(0).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), contents);
        std::env::set_current_dir(directory.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
