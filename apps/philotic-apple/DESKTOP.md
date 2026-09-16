# Mac hotel desktop

The Desktop tab embeds the selected hotel's Philotic Web desktop in WKWebView.
Enter the hotel base URL and choose Attach. The initial suggestion comes from
the native connection settings; changing it does not change the native agent connection.
Reload refreshes the page, Open in Browser opens the selected hotel externally,
and Detach closes the embedded view without logging out. Use the desktop's
sign-out action to end an operator session.

The RootView owns the desktop session and retains its WKWebView across tab
changes. WebKit's persistent data store retains origin-scoped cookies. Device
bearer tokens are never injected into the web view. Desktop management requires
the hotel's separate operator login. External user-clicked HTTP(S) links open
in the system browser; automatic cross-origin top-level navigation is blocked.
External-browser login does not automatically transfer its session into WebKit.

Only HTTP(S) origins without embedded credentials are accepted. Paths, queries,
and fragments are stripped before saving an address. The existing exact-host
ATS exception supports the private vps-jane endpoint over Tailscale; other
hotels should use HTTPS.

## Verification and remaining server work (2026-09-16)

The Mac build and live Desktop tab load the Likes OS HTML at vps-jane:7700.
The live server returns HTML for its referenced JavaScript asset, leaving the
desktop blank. The app detects that missing-bundle condition and displays an
actionable error. The unauthenticated operator login-status API also returns
401; source inspection found bootstrap behind the same operator-session gate.
Repairing/deploying the remote bundle and login flow is separate work. No
authenticated manager action has been verified in this embedded view.

Initial scope excludes file upload/download UI, popup windows, automatic SSO,
and a native-to-JavaScript command bridge.
