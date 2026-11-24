//! `proxlet cert`: get, list, remove and renew certificates.
//!
//! Issuing needs the running server, which answers the ACME challenge; the
//! checks before issuance run here (design decision 3). Listing and removing
//! read the store directly, so they work while proxlet is stopped.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use proxlet_cert::{Store, check_public, renew};
use proxlet_core::settings::AcmeServer;
use proxlet_core::{ConfigFile, Locations, Settings};

use crate::attach;
use crate::control::{Client, Reply, Request};
use crate::output::{self, BOLD, CmdResult, DIM, Failure, WARN};

/// Test hook: resolve names from `name=ip[:port];…` instead of DNS. Names not
/// listed do not resolve. A port also redirects the reachability probe.
pub const RESOLVE_ENV: &str = "PROXLET_TEST_RESOLVE";

/// How long the CLI waits for the server to get one certificate.
pub const ISSUE_TIMEOUT: Duration = Duration::from_secs(150);

/// The certificate store for these locations.
#[must_use]
pub fn store(locations: &Locations) -> Store {
    Store::new(locations.state_dir.join("certs"))
}

fn settings(locations: &Locations) -> Result<Settings, Failure> {
    Ok(ConfigFile::new(locations.config_file()).read()?.1.settings)
}

/// What to call the CA in messages.
fn ca_name(server: &AcmeServer) -> &'static str {
    match server {
        AcmeServer::Production | AcmeServer::Staging => "Let's Encrypt",
        AcmeServer::Url(_) => "the ACME server",
    }
}

/// `proxlet cert <domain>…`; with no domain, lists the certificates.
pub fn issue(locations: &Locations, domains: &[String], yes: bool) -> CmdResult {
    if domains.is_empty() {
        return ls(locations);
    }
    let settings = settings(locations)?;
    let ca = ca_name(&settings.cert_server);
    if crate::control::running(&locations.socket()).is_none() {
        return Err(Failure::new(format!(
            "proxlet must be running to answer {ca}'s challenge"
        ))
        .hint("Start it with: proxlet start   (or in the foreground: proxlet run)"));
    }
    let store = store(locations);
    let mut accept_terms = yes;
    let mut failed = 0;
    for raw in domains {
        if let Err(f) = issue_one(locations, &settings, &store, raw, &mut accept_terms) {
            failed += 1;
            let _ = f.report();
        }
    }
    if failed > 0 {
        return Err(Failure::quiet().code(1));
    }
    Ok(())
}

fn issue_one(
    locations: &Locations,
    settings: &Settings,
    store: &Store,
    raw: &str,
    accept_terms: &mut bool,
) -> Result<(), Failure> {
    let ca = ca_name(&settings.cert_server);
    let domain = check_public(raw).map_err(|e| Failure::new(format!("{raw}: {e}")))?;

    if let Ok(Some(existing)) = store.get(&domain)
        && !renew::should_renew(&existing, proxlet_cert::now())
    {
        output::success(format!(
            "{domain} already has a certificate, valid until {}",
            proxlet_cert::date(existing.leaf.not_after)
        ));
        output::note(format!("Renew it now with: proxlet cert renew {domain}"));
        return Ok(());
    }

    let addrs = resolve(&domain, settings.http_port);
    if addrs.is_empty() {
        return Err(
            Failure::new(format!("{domain} does not resolve")).hint(format!(
                "Add an A or AAAA record for {domain} pointing to this machine, then try again"
            )),
        );
    }
    check_front(settings, &domain)?;
    probe(locations, settings, &domain, &addrs)?;

    output::note(format!("Requesting a certificate for {domain} from {ca}…"));
    let reply = loop {
        let reply = call(
            locations,
            &Request::Issue {
                domain: domain.clone(),
                accept_terms: *accept_terms,
            },
        )?;
        let Some(url) = reply.terms.clone() else {
            break reply;
        };
        if !accept_terms_now(ca, &url)? {
            return Err(Failure::new(format!(
                "{ca}'s terms of service were not accepted; nothing was requested"
            )));
        }
        *accept_terms = true;
    };
    match (reply.ok, reply.cert) {
        (true, Some(cert)) => {
            output::success(format!(
                "Certificate for {domain}, valid until {}",
                proxlet_cert::date(cert.not_after)
            ));
            Ok(())
        }
        _ => Err(Failure::new(format!(
            "{domain}: {}",
            reply
                .error
                .unwrap_or_else(|| "no certificate was issued".into())
        ))),
    }
}

/// Asks the user to accept the terms at `url`. Errors without a terminal.
fn accept_terms_now(ca: &str, url: &str) -> Result<bool, Failure> {
    let terms = if url.is_empty() {
        format!("{ca}'s terms of service")
    } else {
        format!("{ca}'s terms of service ({url})")
    };
    match output::confirm(&format!("Do you accept {terms}?")) {
        Some(answer) => Ok(answer),
        None => Err(Failure::new(format!(
            "getting a certificate needs {terms} to be accepted"
        ))
        .hint("Accept them without a prompt with: --yes")),
    }
}

/// Parses the [`RESOLVE_ENV`] map for `domain`.
fn resolve_from_map(map: &str, domain: &str, port: u16) -> Vec<SocketAddr> {
    map.split(';')
        .filter_map(|e| e.split_once('='))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case(domain))
        .filter_map(|(_, addr)| {
            addr.parse::<SocketAddr>().ok().or_else(|| {
                addr.parse::<std::net::IpAddr>()
                    .ok()
                    .map(|ip| SocketAddr::new(ip, port))
            })
        })
        .collect()
}

/// The domain's addresses, from DNS or the test hook.
fn resolve(domain: &str, port: u16) -> Vec<SocketAddr> {
    if let Ok(map) = std::env::var(RESOLVE_ENV) {
        return resolve_from_map(&map, domain, port);
    }
    (domain, port)
        .to_socket_addrs()
        .map(Iterator::collect)
        .unwrap_or_default()
}

/// Fails when nginx serves `domain` on the HTTP port, so the challenge would
/// never reach proxlet.
fn check_front(settings: &Settings, domain: &str) -> Result<(), Failure> {
    let Some((name, cfg)) = attach::front_config(settings) else {
        return Ok(());
    };
    if let Some(claim) = cfg.claims(domain, settings.http_port) {
        return Err(Failure::new(format!(
            "requests for {domain} on port {} do not reach proxlet: {name} serves it ({})",
            settings.http_port, claim.at
        ))
        .hint(format!(
            "Remove {domain} from that {name} server block, or get its certificate through {name}"
        )));
    }
    Ok(())
}

/// Checks that `domain` on the HTTP port is answered by this proxlet. Only a
/// clear answer from another server stops issuance; a probe that cannot be
/// sent from here (hairpin NAT, a firewall) warns and goes on.
fn probe(
    locations: &Locations,
    settings: &Settings,
    domain: &str,
    addrs: &[SocketAddr],
) -> Result<(), Failure> {
    let reply = call(locations, &Request::Probe)?;
    let Some((token, expected)) = reply.probe else {
        return Ok(());
    };
    let addr = addrs[0];
    let path = format!("{}{token}", proxlet_server::CHALLENGE_PREFIX);
    match http_get(addr, domain, &path) {
        Ok(body) if body == expected => Ok(()),
        Ok(_) => Err(Failure::new(format!(
            "{domain} reaches a server other than this proxlet (it resolves to {})",
            addr.ip()
        ))
        .hint(format!(
            "Point {domain}'s DNS at this machine, and make sure port {} reaches proxlet",
            settings.http_port
        ))),
        Err(e) => {
            output::warn(format!(
                "could not check from here that {domain} reaches proxlet ({e}); asking {} anyway",
                ca_name(&settings.cert_server)
            ));
            Ok(())
        }
    }
}

/// A minimal `GET` returning the body of a 200 response, or an empty body for
/// any other status (which still proves that some server answered).
fn http_get(addr: SocketAddr, host: &str, path: &str) -> std::io::Result<String> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: proxlet\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    s.take(64 * 1024).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let ok = text.starts_with("HTTP/1.1 200") || text.starts_with("HTTP/1.0 200");
    Ok(match text.split_once("\r\n\r\n") {
        Some((_, body)) if ok => body.to_owned(),
        _ => String::new(),
    })
}

fn call(locations: &Locations, request: &Request) -> Result<Reply, Failure> {
    Client::connect_with_timeout(&locations.socket(), ISSUE_TIMEOUT)
        .and_then(|mut c| c.call(request))
        .map_err(|e| Failure::new(format!("proxlet is running but did not answer: {e}")))
}

fn no_certificates_yet() {
    anstream::eprintln!(
        "No certificates yet. Get one with: {BOLD}proxlet cert example.com{BOLD:#}"
    );
}

/// `proxlet cert ls`.
pub fn ls(locations: &Locations) -> CmdResult {
    let listing = store(locations).list()?;
    for p in &listing.problems {
        output::warn(format!("cannot read {p}"));
    }
    if listing.certs.is_empty() {
        no_certificates_yet();
        return Ok(());
    }
    let now = proxlet_cert::now();
    let rows: Vec<[String; 4]> = listing
        .certs
        .iter()
        .map(|c| {
            let mut notes = Vec::new();
            if c.meta.server != "production" && !c.meta.server.is_empty() {
                notes.push(c.meta.server.clone());
            }
            if let Some(e) = &c.meta.last_error {
                notes.push(format!("renewal failing: {e}"));
            }
            [
                c.domain.clone(),
                proxlet_cert::date(c.leaf.not_after),
                proxlet_cert::days_left(c.leaf.not_after, now).to_string(),
                notes.join("; "),
            ]
        })
        .collect();
    if output::stdout_is_terminal() {
        anstream::print!("{}", table(&rows));
    } else {
        for [domain, expires, days, note] in &rows {
            println!("{domain}\t{expires}\t{days}\t{note}");
        }
    }
    Ok(())
}

fn table(rows: &[[String; 4]]) -> String {
    use std::fmt::Write as _;
    let width = |i: usize, header: &str| {
        rows.iter()
            .map(|r| r[i].chars().count())
            .chain([header.len()])
            .max()
            .unwrap_or(0)
    };
    let (w0, w1, w2) = (width(0, "DOMAIN"), width(1, "EXPIRES"), width(2, "DAYS"));
    let mut out = format!(
        "{DIM}{:<w0$}  {:<w1$}  {:>w2$}  NOTE{DIM:#}\n",
        "DOMAIN", "EXPIRES", "DAYS"
    );
    for [domain, expires, days, note] in rows {
        let _ = writeln!(
            out,
            "{BOLD}{domain:<w0$}{BOLD:#}  {expires:<w1$}  {days:>w2$}  {WARN}{note}{WARN:#}"
        );
    }
    out
}

fn unknown(domain: &str, store: &Store) -> Failure {
    let known: Vec<String> = store
        .list()
        .map(|l| l.certs.into_iter().map(|c| c.domain).collect())
        .unwrap_or_default();
    let f = Failure::new(format!("there is no certificate for {domain}"));
    if known.is_empty() {
        f.hint("There are no certificates yet")
    } else {
        f.hint(format!("Certificates exist for: {}", known.join(", ")))
    }
}

fn normalize(raw: &str) -> String {
    raw.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// `proxlet cert rm <domain>…`.
pub fn rm(locations: &Locations, domains: &[String]) -> CmdResult {
    let store = store(locations);
    for raw in domains {
        let domain = normalize(raw);
        if check_public(&domain).is_err() || !store.remove(&domain)? {
            return Err(unknown(raw, &store));
        }
        output::success(format!("Removed the certificate for {domain}"));
    }
    if let Ok(mut c) = Client::connect(&locations.socket()) {
        let _ = c.call(&Request::CertsChanged);
    }
    Ok(())
}

/// `proxlet cert renew [<domain>…]`.
pub fn renew(locations: &Locations, domains: &[String]) -> CmdResult {
    let store = store(locations);
    let mut wanted = Vec::new();
    for raw in domains {
        let domain = normalize(raw);
        if check_public(&domain).is_err() || store.get(&domain).ok().flatten().is_none() {
            return Err(unknown(raw, &store));
        }
        wanted.push(domain);
    }
    if crate::control::running(&locations.socket()).is_none() {
        return Err(
            Failure::new("proxlet must be running to renew certificates")
                .hint("Start it with: proxlet start   (or in the foreground: proxlet run)"),
        );
    }
    if wanted.is_empty() && store.list()?.certs.is_empty() {
        no_certificates_yet();
        return Ok(());
    }
    output::note("Renewing…");
    let reply = call(locations, &Request::Renew { domains: wanted })?;
    let mut failed = false;
    for r in reply.renewed.unwrap_or_default() {
        if let Some(at) = r.not_after {
            output::success(format!(
                "Renewed the certificate for {}, valid until {}",
                r.domain,
                proxlet_cert::date(at)
            ));
        } else {
            failed = true;
            let why = r.error.unwrap_or_else(|| "renewal failed".into());
            let _ = Failure::new(format!("{}: {why}", r.domain)).report();
        }
    }
    if failed {
        return Err(Failure::quiet().code(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_hook_map() {
        let map = "a.example.com=127.0.0.1;b.example.com=127.0.0.1:9";
        assert_eq!(
            resolve_from_map(map, "A.example.com", 80),
            ["127.0.0.1:80".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(
            resolve_from_map(map, "b.example.com", 80),
            ["127.0.0.1:9".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(resolve_from_map(map, "c.example.com", 80), Vec::new());
    }

    #[test]
    fn table_has_a_header_and_aligned_days() {
        let row = |a: &str, b: &str, c: &str, d: &str| [a, b, c, d].map(str::to_owned);
        let rows = vec![
            row("a.example.com", "2027-01-01", "60", ""),
            row("bb.example.com", "2026-12-01", "9", "staging"),
        ];
        let text = anstream::adapter::strip_str(&table(&rows)).to_string();
        assert_eq!(
            text,
            "DOMAIN          EXPIRES     DAYS  NOTE\n\
             a.example.com   2027-01-01    60  \n\
             bb.example.com  2026-12-01     9  staging\n"
        );
    }
}
