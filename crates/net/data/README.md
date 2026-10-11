# OAuth 2.0 provider metadata

`oauth-providers.json` lists mail providers that require or offer OAuth 2.0 sign-in. It holds **public
metadata only**: the authorization, token and device-authorization endpoints the providers publish, the
scopes their IMAP/POP/SMTP servers document, and the mail domains they serve. It is compiled into
`mailcraft-net` (`oauth::builtin_providers`).

**No client ids or client secrets are checked in.** OAuth providers only issue tokens to registered
applications, and a registration belongs to whoever distributes the app. To sign in with OAuth:

1. Register an application with the provider (for example as a "desktop" or "public client" application),
   allowing the loopback redirect `http://127.0.0.1` (any port) and, where offered, the device flow.
2. In MailCraft, open **File › Account Settings**, pick the account, choose **Sign in with OAuth**, pick the
   provider and paste the client id (and, for providers that issue one to installed apps, the client secret).
3. Choose **Device code** (enter the shown code at the provider's verification page on any device) or
   **Browser** (MailCraft opens the provider's page and receives the answer on a one-time 127.0.0.1 port,
   with PKCE).

The access token and the refresh token are kept in the secret store (the OS keyring when available) and never
in the mailbox. The send/receive worker refreshes the access token before it expires.

A provider not listed here can be used with `"provider": "custom"` and its endpoints given explicitly
(`accounts.oauthSignIn` parameters).

Notes on the listed providers (from their public documentation):

- Google: the device flow does not allow the Gmail scope, so only the browser flow is offered. Google issues a
  client secret to desktop applications; it is entered by the user like the client id.
- Microsoft: both flows work with the `common` tenant; `offline_access` is needed for refresh tokens.
