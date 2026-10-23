// Kept std-only so the pinned address fix can be tested without adding the
// entire vendored dependency to Nova's workspace.
pub(crate) fn endpoint(
    host: &str,
    port: u16,
) -> std::io::Result<(Option<std::net::SocketAddr>, String)> {
    let bare = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    let (ip_host, scope) = if let Some((ip, scope)) = bare.rsplit_once('%') {
        (
            ip,
            scope.parse::<u32>().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid IPv6 interface index",
                )
            })?,
        )
    } else {
        (bare, 0)
    };
    match ip_host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) if scope == 0 => Ok((
            Some(std::net::SocketAddr::new(ip.into(), port)),
            ip.to_string(),
        )),
        Ok(std::net::IpAddr::V6(ip)) => Ok((
            Some(std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
                ip, port, 0, scope,
            ))),
            ip.to_string(),
        )),
        _ if scope != 0 || bare.contains(':') || bare.contains('%') => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid receiver IP address",
        )),
        _ => Ok((None, host.to_owned())),
    }
}
#[cfg(test)]
mod endpoint_tests {
    use super::*;
    #[test]
    fn ipv6_scope_is_retained_only_for_the_socket() {
        let (socket, tls) = endpoint("fe80::1234%7", 8009).unwrap();
        assert_eq!(socket.unwrap().to_string(), "[fe80::1234%7]:8009");
        assert_eq!(tls, "fe80::1234");
        assert_eq!(
            endpoint("[::1]", 8009).unwrap().0.unwrap().to_string(),
            "[::1]:8009"
        );
        assert_eq!(
            endpoint("127.0.0.1", 8009).unwrap().0.unwrap().to_string(),
            "127.0.0.1:8009"
        );
        assert!(endpoint("fe80::1234%bogus", 8009).is_err());
        assert_eq!(endpoint("receiver.local", 8009).unwrap().0, None);
    }
}
