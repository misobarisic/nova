# Nova oxicast patch

Source: crates.io `oxicast 0.0.3`, upstream commit
`99e41cb1531a43190713e9a3f4ad50ac52c79b96`, MIT OR Apache-2.0.
The two upstream license files are retained unchanged.

Narrow local changes:

- `src/client/connection.rs` and its std-only `connection_endpoint.rs`: construct literal IPv4/IPv6 socket addresses
  directly, preserve numeric IPv6 scope IDs, and strip scope/brackets only for
  the TLS server name. Add endpoint parsing regression tests.
- Select the ring TLS provider explicitly in `connection.rs`, matching its
  signature verifier, so Cargo enabling both ring and aws-lc through other
  networking dependencies cannot make default-provider selection panic.
- `src/client.rs` and `src/client/router.rs`: omit raw inbound/outbound payloads, launch responses and receiver-provided error reasons from logging. Media
  status and load payloads can contain Nova's short-lived relay bearer tokens.

Nova disables automatic reconnection and closes transport by dropping its final
CastClient handle. The upstream ClientInner drop cancels reader, writer and
heartbeat tasks. It does not send STOP. Upstream `disconnect()` sends STOP and
must not be used for harmless transport detachment.

The endpoint parser tests can run without making oxicast a workspace member:

```sh
rustc --edition=2024 --test vendor/oxicast/src/client/connection_endpoint.rs -o /tmp/nova-oxicast-endpoint-test
/tmp/nova-oxicast-endpoint-test
```
