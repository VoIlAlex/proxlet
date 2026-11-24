//! A reader for the parts of nginx's config language proxlet needs.
//!
//! It tokenizes like nginx (words, quotes, `;`, blocks, `#` comments), expands
//! `include` (with `*` and `?` in the file name) relative to the main config's
//! directory, and collects `server` blocks in the `http` context with their
//! `listen` and `server_name` directives. Everything else is skipped.

use std::path::{Path, PathBuf};

use crate::FrontError;
use crate::config::{FrontConfig, IncludeDir, Listen, Location, ServerBlock, ServerName};

/// The first line of every file proxlet writes into nginx's config.
pub const MANAGED_MARKER: &str = "# Managed by proxlet.";

const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Semi,
    Open,
    Close,
}

#[derive(Debug, Clone)]
struct Directive {
    name: String,
    args: Vec<String>,
    at: Location,
    block: Option<Vec<Directive>>,
}

fn tokenize(src: &str, file: &Path) -> Result<Vec<(Tok, usize)>, FrontError> {
    let err = |line, message: &str| FrontError::Parse {
        at: Location {
            file: file.to_owned(),
            line,
        },
        message: message.into(),
    };
    let mut out = Vec::new();
    let mut chars = src.chars().peekable();
    let mut line = 1;
    while let Some(&c) = chars.peek() {
        match c {
            '\n' => {
                line += 1;
                chars.next();
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            '#' => {
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            }
            ';' => {
                out.push((Tok::Semi, line));
                chars.next();
            }
            '{' => {
                out.push((Tok::Open, line));
                chars.next();
            }
            '}' => {
                out.push((Tok::Close, line));
                chars.next();
            }
            '"' | '\'' => {
                let start = line;
                chars.next();
                let mut word = String::new();
                loop {
                    match chars.next() {
                        None => return Err(err(start, "unterminated quote")),
                        Some('\\') => {
                            if let Some(n) = chars.next() {
                                if n == '\n' {
                                    line += 1;
                                }
                                word.push(n);
                            }
                        }
                        Some(q) if q == c => break,
                        Some(n) => {
                            if n == '\n' {
                                line += 1;
                            }
                            word.push(n);
                        }
                    }
                }
                out.push((Tok::Word(word), start));
            }
            _ => {
                let mut word = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || n == ';' || n == '}' {
                        break;
                    }
                    if n == '{' {
                        // `${var}` is part of a word; a bare `{` opens a block.
                        if !word.ends_with('$') {
                            break;
                        }
                        for v in chars.by_ref() {
                            word.push(v);
                            if v == '}' {
                                break;
                            }
                        }
                        continue;
                    }
                    if n == '\\' {
                        chars.next();
                        if let Some(e) = chars.next() {
                            word.push(e);
                        }
                        continue;
                    }
                    word.push(n);
                    chars.next();
                }
                out.push((Tok::Word(word), line));
            }
        }
    }
    Ok(out)
}

fn parse(src: &str, file: &Path) -> Result<Vec<Directive>, FrontError> {
    let toks = tokenize(src, file)?;
    let mut pos = 0;
    parse_block(&toks, &mut pos, file, false)
}

fn parse_block(
    toks: &[(Tok, usize)],
    pos: &mut usize,
    file: &Path,
    nested: bool,
) -> Result<Vec<Directive>, FrontError> {
    let at = |line| Location {
        file: file.to_owned(),
        line,
    };
    let mut out = Vec::new();
    let mut words: Vec<(String, usize)> = Vec::new();
    while *pos < toks.len() {
        let (tok, line) = &toks[*pos];
        *pos += 1;
        match tok {
            Tok::Word(w) => words.push((w.clone(), *line)),
            Tok::Semi => {
                if let Some(d) = directive(std::mem::take(&mut words), file, None) {
                    out.push(d);
                }
            }
            Tok::Open => {
                let start = words.first().map_or(*line, |w| w.1);
                let block = parse_block(toks, pos, file, true)?;
                let Some(d) = directive(std::mem::take(&mut words), file, Some(block)) else {
                    return Err(FrontError::Parse {
                        at: at(start),
                        message: "block without a name".into(),
                    });
                };
                out.push(d);
            }
            Tok::Close => {
                if !nested {
                    return Err(FrontError::Parse {
                        at: at(*line),
                        message: "unexpected `}`".into(),
                    });
                }
                return Ok(out);
            }
        }
    }
    if nested {
        let line = toks.last().map_or(1, |t| t.1);
        return Err(FrontError::Parse {
            at: at(line),
            message: "unexpected end of file, expecting `}`".into(),
        });
    }
    if let Some((_, line)) = words.first() {
        return Err(FrontError::Parse {
            at: at(*line),
            message: "unexpected end of file, expecting `;`".into(),
        });
    }
    Ok(out)
}

fn directive(
    words: Vec<(String, usize)>,
    file: &Path,
    block: Option<Vec<Directive>>,
) -> Option<Directive> {
    let mut words = words.into_iter();
    let (name, line) = words.next()?;
    Some(Directive {
        name,
        args: words.map(|w| w.0).collect(),
        at: Location {
            file: file.to_owned(),
            line,
        },
        block,
    })
}

/// Reads the config rooted at `conf_path`.
pub(crate) fn load(conf_path: &Path) -> Result<FrontConfig, FrontError> {
    let mut reader = Reader {
        base: conf_path.parent().unwrap_or(Path::new("/")).to_owned(),
        cfg: FrontConfig {
            conf_path: conf_path.to_owned(),
            ..FrontConfig::default()
        },
    };
    let (directives, managed) = read_file(conf_path)?;
    reader.visit(&directives, Ctx::Main, managed, 0)?;
    Ok(reader.cfg)
}

fn read_file(path: &Path) -> Result<(Vec<Directive>, bool), FrontError> {
    let src = std::fs::read_to_string(path).map_err(|source| FrontError::Io {
        path: path.to_owned(),
        source,
    })?;
    let managed = src.starts_with(MANAGED_MARKER);
    Ok((parse(&src, path)?, managed))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Main,
    Http,
}

struct Reader {
    base: PathBuf,
    cfg: FrontConfig,
}

impl Reader {
    fn resolve(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_owned()
        } else {
            self.base.join(p)
        }
    }

    /// The files an `include` loads, in nginx's (sorted) order.
    fn include(&self, d: &Directive, depth: usize) -> Result<Vec<PathBuf>, FrontError> {
        if depth >= MAX_DEPTH {
            return Err(FrontError::Parse {
                at: d.at.clone(),
                message: "includes are nested too deeply".into(),
            });
        }
        let Some(arg) = d.args.first() else {
            return Ok(Vec::new());
        };
        let path = self.resolve(arg);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !is_glob(&name) {
            return Ok(vec![path]);
        }
        let dir = path.parent().unwrap_or(Path::new("/"));
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(Vec::new());
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
            .filter(|e| glob_match(&name, &e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
        files.sort();
        Ok(files)
    }

    fn visit(
        &mut self,
        directives: &[Directive],
        ctx: Ctx,
        managed: bool,
        depth: usize,
    ) -> Result<(), FrontError> {
        for d in directives {
            match (d.name.as_str(), ctx) {
                ("include", _) => {
                    if ctx == Ctx::Http
                        && let Some(arg) = d.args.first()
                    {
                        let path = self.resolve(arg);
                        let pattern = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if is_glob(&pattern) {
                            self.cfg.http_includes.push(IncludeDir {
                                dir: path.parent().unwrap_or(Path::new("/")).to_owned(),
                                pattern,
                                at: d.at.clone(),
                            });
                        }
                    }
                    for file in self.include(d, depth)? {
                        let (inner, inner_managed) = read_file(&file)?;
                        self.visit(&inner, ctx, inner_managed, depth + 1)?;
                    }
                }
                ("user", Ctx::Main) => {
                    self.cfg.worker_user = d.args.first().cloned();
                    self.cfg.worker_group = d.args.get(1).cloned();
                }
                ("error_log", Ctx::Main) => {
                    if let Some(p) = d.args.first() {
                        self.cfg.error_log = Some(p.clone());
                    }
                }
                ("http", Ctx::Main) => {
                    if let Some(block) = &d.block {
                        self.visit(block, Ctx::Http, managed, depth)?;
                    }
                }
                ("server", Ctx::Http) => {
                    if let Some(block) = &d.block {
                        let mut server = ServerBlock {
                            listens: Vec::new(),
                            names: Vec::new(),
                            at: d.at.clone(),
                            managed,
                        };
                        self.collect_server(block, &mut server, depth)?;
                        self.cfg.servers.push(server);
                    }
                }
                // `events`, `stream`, `mail`, `location`...: nothing proxlet needs.
                _ => {}
            }
        }
        Ok(())
    }

    fn collect_server(
        &self,
        directives: &[Directive],
        server: &mut ServerBlock,
        depth: usize,
    ) -> Result<(), FrontError> {
        for d in directives {
            match d.name.as_str() {
                "listen" => {
                    if let Some(listen) = listen(d) {
                        server.listens.push(listen);
                    }
                }
                "server_name" => server
                    .names
                    .extend(d.args.iter().filter(|n| !n.is_empty()).map(|n| ServerName {
                        name: n.clone(),
                        at: d.at.clone(),
                    })),
                "include" => {
                    for file in self.include(d, depth)? {
                        let (inner, _) = read_file(&file)?;
                        self.collect_server(&inner, server, depth + 1)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Parses a `listen` directive; `None` for unix sockets.
fn listen(d: &Directive) -> Option<Listen> {
    let addr = d.args.first()?;
    if addr.starts_with("unix:") {
        return None;
    }
    let port = if addr.chars().all(|c| c.is_ascii_digit()) {
        addr.parse().ok()?
    } else if let Some(rest) = addr.strip_prefix('[') {
        match rest.split_once("]:") {
            Some((_, p)) => p.parse().ok()?,
            None => 80,
        }
    } else {
        match addr.rsplit_once(':') {
            Some((_, p)) => p.parse().ok()?,
            None => 80,
        }
    };
    let default_flag = d
        .args
        .iter()
        .skip(1)
        .find(|a| *a == "default_server" || *a == "default")
        .cloned();
    Some(Listen {
        port,
        ipv6: addr.starts_with('['),
        default_flag,
        at: d.at.clone(),
    })
}

fn is_glob(name: &str) -> bool {
    name.contains(['*', '?'])
}

/// `*` and `?` wildcards; a leading dot is not special (like nginx's `glob`).
fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(src: &str) -> Vec<Tok> {
        tokenize(src, Path::new("t"))
            .unwrap()
            .into_iter()
            .map(|t| t.0)
            .collect()
    }

    fn w(s: &str) -> Tok {
        Tok::Word(s.into())
    }

    #[test]
    fn tokenizes_quotes_comments_and_variables() {
        assert_eq!(
            words("server_name \"a b\" 'c;d' e\\;f; # comment ; {\nreturn 200 ${host}x;"),
            [
                w("server_name"),
                w("a b"),
                w("c;d"),
                w("e;f"),
                Tok::Semi,
                w("return"),
                w("200"),
                w("${host}x"),
                Tok::Semi,
            ]
        );
    }

    #[test]
    fn reports_structural_errors_with_lines() {
        for (src, line, msg) in [
            ("a;\n}", 2, "unexpected `}`"),
            ("http {\n  server {\n", 2, "expecting `}`"),
            ("a b", 1, "expecting `;`"),
            ("a \"open\n", 1, "unterminated quote"),
        ] {
            let err = parse(src, Path::new("/x.conf")).unwrap_err();
            let text = err.to_string();
            assert!(text.contains(&format!("/x.conf:{line}")), "{src:?}: {text}");
            assert!(text.contains(msg), "{src:?}: {text}");
        }
    }

    fn listen_of(args: &[&str]) -> Option<Listen> {
        listen(&Directive {
            name: "listen".into(),
            args: args.iter().map(|a| (*a).into()).collect(),
            at: Location {
                file: "f".into(),
                line: 1,
            },
            block: None,
        })
    }

    #[test]
    fn listen_forms() {
        let port = |args: &[&str]| listen_of(args).map(|l| l.port);
        assert_eq!(port(&["80"]), Some(80));
        assert_eq!(port(&["8080"]), Some(8080));
        assert_eq!(port(&["127.0.0.1:8080"]), Some(8080));
        assert_eq!(port(&["*:80"]), Some(80));
        assert_eq!(port(&["localhost"]), Some(80));
        assert_eq!(port(&["[::]:80"]), Some(80));
        assert_eq!(port(&["[::1]"]), Some(80));
        assert_eq!(port(&["unix:/run/x.sock"]), None);
        assert_eq!(
            listen_of(&["80", "default_server"])
                .unwrap()
                .default_flag
                .as_deref(),
            Some("default_server")
        );
        assert_eq!(
            listen_of(&["80", "default"])
                .unwrap()
                .default_flag
                .as_deref(),
            Some("default")
        );
        assert_eq!(listen_of(&["443", "ssl"]).unwrap().default_flag, None);
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.conf", "a.conf"));
        assert!(glob_match("*.conf", ".conf"));
        assert!(!glob_match("*.conf", "a.conf.bak"));
        assert!(glob_match("*", "default"));
        assert!(glob_match("site?.conf", "site1.conf"));
        assert!(!glob_match("site?.conf", "site.conf"));
    }
}
