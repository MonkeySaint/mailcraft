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
- Microsoft: both flows work with the `common` tenant (or the organization's own tenant, which account setup
  finds from the domain); `offline_access` is needed for refresh tokens. A **Microsoft 365 (Graph)** account
  signs in with the same provider but asks for Microsoft Graph's delegated scopes instead of the scopes listed
  in `oauth-providers.json` (`mailcraft_net::graph::MAIL_SCOPES`): `https://graph.microsoft.com/Mail.ReadWrite`,
  `https://graph.microsoft.com/Mail.Send` and `offline_access`. One access token serves one resource, so a
  Graph account's token never signs in to IMAP or SMTP, and the other way round.

## Registering a client id, step by step

Whoever builds and distributes a copy of MailCraft (or each user, for their own use) registers it once with
each provider. MailCraft is a *public client* (an installed application cannot keep a secret), so register
it as one:

**Any provider**

- Application type: *desktop*, *native*, *installed* or *public client* (not "web application").
- Redirect URI: `http://127.0.0.1` with any port (RFC 8252 section 7.3). MailCraft listens on a one-time
  port of 127.0.0.1 and sends `http://127.0.0.1:<port>/` as the redirect URI; providers that require the
  port to be listed cannot be used with the browser flow (use the device flow instead).
- Device flow: enable it if the provider has a switch for it (RFC 8628).
- Scopes: the IMAP/POP/SMTP scopes the provider documents (see `oauth-providers.json`), plus whatever the
  provider needs to issue refresh tokens.
- Note the client id (and, only if the provider issues one to installed apps, the client secret).

**Google** (public documentation: "OAuth 2.0 for desktop apps", "IMAP, POP and SMTP: OAuth")

1. In the Google Cloud console, create a project and configure the OAuth consent screen (add yourself as a
   test user while the app is unverified).
2. Create credentials › OAuth client ID › Application type **Desktop app**. Google shows a client id and a
   client secret; enter both in MailCraft.
3. Use the **Browser** flow: Google's device flow does not allow the `https://mail.google.com/` scope.

**Microsoft** (public documentation: "Register an application with the Microsoft identity platform",
"Authenticate an IMAP, POP or SMTP connection using OAuth")

1. In the Azure portal (Microsoft Entra ID › App registrations) create a registration; supported account
   types: personal and work or school accounts, to match the `common` endpoints.
2. Authentication › Add a platform › **Mobile and desktop applications**, and add **both** redirect URIs
   `http://127.0.0.1` and `http://localhost`. MailCraft sends `http://127.0.0.1:<port>/` (a one-time port);
   Microsoft ignores the port of loopback redirects for public clients, but only matches the host name that
   is registered, so `http://localhost` alone is not enough.
3. Authentication › **Allow public client flows: Yes** (needed for the device code flow).
4. API permissions: `IMAP.AccessAsUser.All`, `POP.AccessAsUser.All`, `SMTP.Send` (Office 365 Exchange
   Online) and `offline_access` for IMAP, POP and SMTP accounts; for **Microsoft 365 (Graph)** accounts,
   Microsoft Graph › Delegated permissions › `Mail.ReadWrite`, `Mail.Send` and `offline_access` (calendars,
   contacts and tasks over Graph add their own). One registration can carry both sets. No client secret is
   needed.
5. Supported account types: a registration for your own organization only (*single tenant*) is fine. Account
   setup finds the organization's tenant id from its domain and signs in at
   `https://login.microsoftonline.com/<tenant>/…` instead of `common` (`"tenant"` in `accounts.oauthSignIn`).
   A registration in one organization used by people of another needs consent by an administrator of theirs.

Microsoft 365 mailboxes also need, on the organization's side: IMAP (or POP) and **Authenticated SMTP**
turned on for the mailbox (both can be off per mailbox or for the whole organization), and, where users may
not consent to apps themselves, an administrator's consent to the permissions above. Basic authentication
(a password) is off for IMAP, POP and SMTP on Microsoft 365 and Outlook.com, so OAuth is the only way in.

**Another provider** (`"provider": "custom"`): give its endpoints with the client id, for example

```json
{"account": 3, "provider": "custom", "clientId": "<your client id>", "flow": "device",
 "tokenEndpoint": "https://auth.example.net/token",
 "deviceAuthorizationEndpoint": "https://auth.example.net/device",
 "authorizationEndpoint": "https://auth.example.net/authorize",
 "scopes": ["mail"]}
```

Endpoints must be `https`. The IMAP, POP3 and SMTP servers must offer `AUTH=XOAUTH2` or `AUTH=OAUTHBEARER`
(RFC 7628); JMAP servers take the access token as a Bearer token.

## Trying it without a real provider

`mailcraft-testmail` runs a local authorization server (`TestServers::oauth_endpoints`, client id
`mailcraft-test-client`) whose access tokens sign in to its IMAP, SMTP and JMAP servers and its fake
Microsoft Graph mail service. Tests approve a
device code with `TestServers::oauth_approve` (standing in for the person at the verification page) and can
expire every access token at once to exercise refreshing. See `crates/net/tests/accounts_testmail.rs` and
`crates/engine/tests/e2e_setup.rs`.
