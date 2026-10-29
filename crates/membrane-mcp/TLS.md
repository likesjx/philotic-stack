# Direct proxy transport

Opt in with `--tls-config /absolute/config.json --bind-address 172.18.0.1`.
The JSON has exactly `certificate`, `private_key`, `client_ca` (absolute PEM file
paths) and `allowed_client_sha256` (1–8 lowercase SHA256 leaf certificate pins).
Private keys must exclude group/other permissions. Configuration loads before IPC;
invalid files/configuration terminate startup, never fall back to HTTP. Rotation
requires a supervised guest restart. Pins authenticate the ingress proxy, not a
user, owner or OAuth grant.

TLS1.3 and HTTP/1.1 are supported. Client certificates require both CA validation
and an allowed leaf pin. HTTP/2 is not advertised. Proxy transport is internally
marked remote even over loopback and without forwarding headers. Perimeter
narrowing stops accepting and cancels/awaits existing TLS connection tasks before
rebinding. A committed memory operation is not retroactively rolled back by that
connection cancellation; the existing hotel authority/receipt rules still apply.

Explicit nonlocal binding requires hotel-managed TLS and a private IPv4 address;
wildcards, multicast and public addresses are refused. The hotel's perimeter and
endpoint exposure still constrain widening. If either narrows, the listener moves
to loopback while retaining TLS. Existing deployments that omit both options keep
their prior HTTP/perimeter behavior. The native protected deployment must require
both options; this is not an automatic upgrade of legacy listeners.

Production approval must separately cover the bridge listener, proxy certificate
custody, exact native Traefik route/transport and any Traefik recreation affecting
other public routes. No production settings are changed by this implementation.
