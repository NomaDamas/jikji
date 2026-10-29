//! Access gate for the GUI: nothing but the login page is served without a valid session.
//!
//! The console reaches files, previews and management actions, so it is treated as private even
//! on loopback (a tunnel or proxy in front of it makes loopback reachable from the internet).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use super::http::{HttpRequest, HttpResponse};
use super::token::ManagementToken;

pub(crate) const SESSION_COOKIE: &str = "jikji_session";
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_FAILED_LOGINS: usize = 5;
/// Failed logins allowed from all clients together per lockout window. Forwarded client
/// addresses can be forged by anyone who reaches the loopback port directly, so this global
/// budget is what actually bounds token guessing.
const MAX_FAILED_LOGINS_GLOBAL: usize = 30;
const LOCKOUT: Duration = Duration::from_secs(10 * 60);
const MAX_SESSIONS: usize = 256;
/// Distinct client addresses tracked for per-client lockout; older ones are dropped first.
const MAX_TRACKED_CLIENTS: usize = 1024;
const GLOBAL_KEY: &str = "\u{0}global";

const LOGIN_HTML: &str = include_str!("assets/login.html");

/// Headers sent with every response. The console is one file with inline style and script, so
/// those two need `'unsafe-inline'`; everything else is same-origin only, and framing is denied.
pub(crate) fn security_headers() -> Vec<(&'static str, String)> {
    vec![
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; media-src 'self' blob:; object-src 'self' blob:; frame-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
                .to_owned(),
        ),
        ("X-Content-Type-Options", "nosniff".to_owned()),
        ("X-Frame-Options", "DENY".to_owned()),
        ("Referrer-Policy", "no-referrer".to_owned()),
        ("Cross-Origin-Opener-Policy", "same-origin".to_owned()),
        ("Cross-Origin-Resource-Policy", "same-origin".to_owned()),
    ]
}

#[derive(Clone)]
pub(crate) struct AccessGate {
    access_token: ManagementToken,
    /// Extra `Host` header values accepted besides loopback (e.g. the tunnel hostname).
    allowed_hosts: Arc<Vec<String>>,
    sessions: Arc<Mutex<HashMap<String, Instant>>>,
    failures: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
}

pub(crate) enum Gate {
    /// The request may reach the console.
    Allow,
    /// The gate answered the request itself (login page, login result, rejection).
    Respond(HttpResponse),
}

impl AccessGate {
    pub(crate) fn new(access_token: ManagementToken, allowed_hosts: Vec<String>) -> Self {
        Self {
            access_token,
            allowed_hosts: Arc::new(
                allowed_hosts
                    .into_iter()
                    .map(|host| host.trim().to_ascii_lowercase())
                    .filter(|host| !host.is_empty())
                    .collect(),
            ),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            failures: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn check(&self, request: &HttpRequest) -> Gate {
        if !self.host_allowed(request.header("host")) {
            return Gate::Respond(HttpResponse::json(421, json!({"error": "unknown host"})));
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/login") => return Gate::Respond(self.login(request)),
            ("POST", "/api/logout") => return Gate::Respond(self.logout(request)),
            _ => {}
        }
        if request.method == "GET" && request.path == "/" {
            if let Some(token) = request.param("access") {
                return Gate::Respond(self.link_login(request, &token));
            }
        }
        if self.session_valid(request) {
            return Gate::Allow;
        }
        if request.method == "GET" && matches!(request.path.as_str(), "/" | "/index.html") {
            return Gate::Respond(HttpResponse::html(200, LOGIN_HTML));
        }
        Gate::Respond(HttpResponse::json(401, json!({"error": "login required"})))
    }

    fn host_allowed(&self, host: Option<&str>) -> bool {
        let Some(host) = host else {
            return false;
        };
        let host = host.trim().to_ascii_lowercase();
        let name = match host.strip_prefix('[') {
            Some(rest) => rest.split(']').next().unwrap_or_default().to_owned(),
            None => host
                .rsplit_once(':')
                .map_or(host.as_str(), |(name, _)| name)
                .to_owned(),
        };
        matches!(name.as_str(), "127.0.0.1" | "localhost" | "::1")
            || self.allowed_hosts.iter().any(|allowed| allowed == &name)
    }

    fn login(&self, request: &HttpRequest) -> HttpResponse {
        let client = client_key(request);
        if self.locked_out(&client) {
            return HttpResponse::json(
                429,
                json!({"error": "too many failed attempts; wait 10 minutes"}),
            );
        }
        let candidate = request.param("token").unwrap_or_default();
        if !self.access_token.matches(candidate.trim()) {
            self.record_failure(client);
            return HttpResponse::json(401, json!({"error": "invalid access token"}));
        }
        self.clear_failures(&client);
        match self.open_session() {
            Some(session) => HttpResponse::empty(204)
                .with_header("Set-Cookie", session_cookie(&session, request.is_https())),
            None => HttpResponse::json(500, json!({"error": "session could not be created"})),
        }
    }

    /// `/?access=TOKEN` one-click link: start a session and redirect to `/` without the token.
    fn link_login(&self, request: &HttpRequest, token: &str) -> HttpResponse {
        let client = client_key(request);
        if self.locked_out(&client) {
            return HttpResponse::json(
                429,
                json!({"error": "too many failed attempts; wait 10 minutes"}),
            );
        }
        if !self.access_token.matches(token.trim()) {
            self.record_failure(client);
            return HttpResponse::html(401, LOGIN_HTML);
        }
        self.clear_failures(&client);
        match self.open_session() {
            Some(session) => HttpResponse::empty(303)
                .with_header("Location", "/".to_owned())
                .with_header("Set-Cookie", session_cookie(&session, request.is_https())),
            None => HttpResponse::json(500, json!({"error": "session could not be created"})),
        }
    }

    fn logout(&self, request: &HttpRequest) -> HttpResponse {
        if let (Some(session), Ok(mut sessions)) =
            (request.cookie(SESSION_COOKIE), self.sessions.lock())
        {
            sessions.remove(session);
        }
        let secure = if request.is_https() { "; Secure" } else { "" };
        HttpResponse::empty(204).with_header(
            "Set-Cookie",
            format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}"),
        )
    }

    fn session_valid(&self, request: &HttpRequest) -> bool {
        let Some(session) = request.cookie(SESSION_COOKIE) else {
            return false;
        };
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        match sessions.get(session) {
            Some(created) if created.elapsed() < SESSION_TTL => true,
            Some(_) => {
                sessions.remove(session);
                false
            }
            None => false,
        }
    }

    fn open_session(&self) -> Option<String> {
        let session = ManagementToken::generate().ok()?.as_str().to_owned();
        let mut sessions = self.sessions.lock().ok()?;
        sessions.retain(|_, created| created.elapsed() < SESSION_TTL);
        if sessions.len() >= MAX_SESSIONS {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, created)| **created)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(session.clone(), Instant::now());
        Some(session)
    }

    /// Locked out when this client, or all clients together, failed too often recently.
    /// Expired entries are pruned on every check, so the map cannot grow without bound.
    fn locked_out(&self, client: &str) -> bool {
        let Ok(mut failures) = self.failures.lock() else {
            return true;
        };
        failures.retain(|_, times| {
            times.retain(|time| time.elapsed() < LOCKOUT);
            !times.is_empty()
        });
        let count = |key: &str| failures.get(key).map_or(0, Vec::len);
        count(client) >= MAX_FAILED_LOGINS || count(GLOBAL_KEY) >= MAX_FAILED_LOGINS_GLOBAL
    }

    fn record_failure(&self, client: String) {
        let Ok(mut failures) = self.failures.lock() else {
            return;
        };
        let now = Instant::now();
        failures.entry(GLOBAL_KEY.to_owned()).or_default().push(now);
        if !failures.contains_key(&client) && failures.len() > MAX_TRACKED_CLIENTS {
            let oldest = failures
                .iter()
                .filter(|(key, _)| key.as_str() != GLOBAL_KEY)
                .min_by_key(|(_, times)| times.last().copied())
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                failures.remove(&oldest);
            }
        }
        let times = failures.entry(client).or_default();
        times.push(now);
        if times.len() > MAX_FAILED_LOGINS {
            times.remove(0);
        }
    }

    /// A successful login forgives that client's failures but not the global budget.
    fn clear_failures(&self, client: &str) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.remove(client);
        }
    }

    #[cfg(test)]
    fn tracked_clients(&self) -> usize {
        self.failures
            .lock()
            .map_or(usize::MAX, |failures| failures.len())
    }
}

/// Client identity for attempt limiting. Behind a tunnel every request arrives from loopback,
/// so the forwarded address is used when present.
fn client_key(request: &HttpRequest) -> String {
    request
        .header("cf-connecting-ip")
        .or_else(|| request.header("x-forwarded-for"))
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "local".to_owned())
}

fn session_cookie(session: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure}",
        SESSION_TTL.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::{AccessGate, Gate, SESSION_COOKIE};
    use crate::gui_commands::http::HttpRequest;
    use crate::gui_commands::token::ManagementToken;

    fn request(method: &str, target: &str, headers: &[(&str, &str)], body: &str) -> HttpRequest {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        HttpRequest {
            method: method.to_owned(),
            path: path.to_owned(),
            query: query.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_owned()))
                .collect(),
            body: body.to_owned(),
        }
    }

    fn gate() -> AccessGate {
        AccessGate::new(
            ManagementToken::new("secret-token".to_owned()),
            vec!["share.example.com".to_owned()],
        )
    }

    fn status(outcome: Gate) -> u16 {
        match outcome {
            Gate::Allow => 0,
            Gate::Respond(response) => response.status(),
        }
    }

    fn login_cookie(gate: &AccessGate) -> String {
        let Gate::Respond(response) = gate.check(&request(
            "POST",
            "/api/login",
            &[("Host", "127.0.0.1:1")],
            "token=secret-token",
        )) else {
            panic!("login must answer");
        };
        assert_eq!(response.status(), 204);
        let cookie = response.header_value("Set-Cookie").expect("session cookie");
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        cookie.split(';').next().unwrap().to_owned()
    }

    #[test]
    fn everything_needs_a_session_except_the_login_page() {
        let gate = gate();
        let host = [("Host", "127.0.0.1:8765")];
        assert_eq!(
            status(gate.check(&request("GET", "/", &host, ""))),
            200,
            "login page"
        );
        for path in [
            "/api/status",
            "/api/find?q=x",
            "/api/preview?path=a",
            "/download?path=a",
        ] {
            assert_eq!(
                status(gate.check(&request("GET", path, &host, ""))),
                401,
                "{path}"
            );
        }
        assert_eq!(
            status(gate.check(&request("POST", "/api/refresh", &host, ""))),
            401
        );

        let cookie = login_cookie(&gate);
        let authed = [("Host", "127.0.0.1:8765"), ("Cookie", cookie.as_str())];
        assert!(matches!(
            gate.check(&request("GET", "/api/status", &authed, "")),
            Gate::Allow
        ));
        assert!(matches!(
            gate.check(&request("GET", "/", &authed, "")),
            Gate::Allow
        ));

        let forged = format!("{SESSION_COOKIE}=not-a-session");
        let forged = [("Host", "127.0.0.1:8765"), ("Cookie", forged.as_str())];
        assert_eq!(
            status(gate.check(&request("GET", "/api/status", &forged, ""))),
            401
        );
    }

    #[test]
    fn wrong_tokens_are_rejected_and_rate_limited() {
        let gate = gate();
        let from = [
            ("Host", "share.example.com"),
            ("CF-Connecting-IP", "203.0.113.9"),
        ];
        for _ in 0..5 {
            assert_eq!(
                status(gate.check(&request("POST", "/api/login", &from, "token=guess"))),
                401
            );
        }
        assert_eq!(
            status(gate.check(&request("POST", "/api/login", &from, "token=secret-token"))),
            429,
            "even the right token is refused while locked out"
        );
        let other = [
            ("Host", "share.example.com"),
            ("CF-Connecting-IP", "198.51.100.4"),
        ];
        assert_eq!(
            status(gate.check(&request("POST", "/api/login", &other, "token=secret-token"))),
            204
        );
    }

    #[test]
    fn access_link_sets_a_session_and_strips_the_token() {
        let gate = gate();
        let Gate::Respond(response) = gate.check(&request(
            "GET",
            "/?access=secret-token",
            &[
                ("Host", "share.example.com"),
                ("X-Forwarded-Proto", "https"),
            ],
            "",
        )) else {
            panic!("link login must answer");
        };
        assert_eq!(response.status(), 303);
        assert_eq!(response.header_value("Location"), Some("/"));
        assert!(
            response
                .header_value("Set-Cookie")
                .unwrap()
                .contains("; Secure")
        );
        assert_eq!(
            status(gate.check(&request(
                "GET",
                "/?access=wrong",
                &[("Host", "127.0.0.1")],
                ""
            ))),
            401
        );
    }

    #[test]
    fn unknown_hosts_and_missing_host_are_refused() {
        let gate = gate();
        assert_eq!(
            status(gate.check(&request("GET", "/", &[("Host", "evil.example")], ""))),
            421
        );
        assert_eq!(status(gate.check(&request("GET", "/", &[], ""))), 421);
        assert_eq!(
            status(gate.check(&request("GET", "/", &[("Host", "[::1]:8765")], ""))),
            200
        );
        assert_eq!(
            status(gate.check(&request("GET", "/", &[("Host", "localhost")], ""))),
            200
        );
    }

    #[test]
    fn logout_ends_the_session() {
        let gate = gate();
        let cookie = login_cookie(&gate);
        let authed = [("Host", "127.0.0.1"), ("Cookie", cookie.as_str())];
        assert_eq!(
            status(gate.check(&request("POST", "/api/logout", &authed, ""))),
            204
        );
        assert_eq!(
            status(gate.check(&request("GET", "/api/status", &authed, ""))),
            401
        );
    }
}

#[cfg(test)]
mod lockout_tests {
    use super::{AccessGate, Gate, MAX_FAILED_LOGINS_GLOBAL, MAX_TRACKED_CLIENTS};
    use crate::gui_commands::http::HttpRequest;
    use crate::gui_commands::token::ManagementToken;

    fn login(gate: &AccessGate, ip: &str, token: &str) -> u16 {
        let request = HttpRequest {
            method: "POST".to_owned(),
            path: "/api/login".to_owned(),
            query: String::new(),
            headers: vec![
                ("host".to_owned(), "127.0.0.1".to_owned()),
                ("cf-connecting-ip".to_owned(), ip.to_owned()),
            ],
            body: format!("token={token}"),
        };
        match gate.check(&request) {
            Gate::Allow => 0,
            Gate::Respond(response) => response.status(),
        }
    }

    #[test]
    fn forged_client_addresses_cannot_escape_the_global_budget() {
        let gate = AccessGate::new(ManagementToken::new("secret-token".to_owned()), Vec::new());
        for attempt in 0..MAX_FAILED_LOGINS_GLOBAL {
            assert_eq!(login(&gate, &format!("10.0.0.{attempt}"), "guess"), 401);
        }
        assert_eq!(
            login(&gate, "10.9.9.9", "guess"),
            429,
            "a fresh forged address is still locked out once the global budget is spent"
        );
    }

    #[test]
    fn tracked_client_map_stays_bounded() {
        let gate = AccessGate::new(ManagementToken::new("secret-token".to_owned()), Vec::new());
        for attempt in 0..MAX_TRACKED_CLIENTS + 200 {
            gate.record_failure(format!("client-{attempt}"));
        }
        let tracked = gate.tracked_clients();
        assert!(
            tracked <= MAX_TRACKED_CLIENTS + 2,
            "tracked {tracked} clients"
        );
    }
}
