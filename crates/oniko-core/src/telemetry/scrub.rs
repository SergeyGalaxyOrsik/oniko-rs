/// What could identify the user in an error message or stack.
#[derive(Debug, Clone)]
pub struct ScrubContext {
    pub home: String,
    pub user: String,
    pub app_path: String,
}

/// JavaScript's `encodeURI`: everything but the URI alphabet is percent-encoded.
fn encode_uri(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || ";,/?:@&=+$-_.!~*'()#".contains(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// The same path as written natively, with forward slashes, and URL-encoded.
fn spellings(path: &str) -> Vec<String> {
    let forward = path.replace('\\', "/");
    let mut out: Vec<String> = Vec::new();
    for p in [path.to_owned(), forward.clone(), encode_uri(&forward)] {
        if p.chars().count() > 1 && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn ci_regex(pattern: &str) -> regex::Regex {
    regex::RegexBuilder::new(pattern).case_insensitive(true).build().expect("escaped pattern")
}

/// Replaces `user` where it is not part of a longer word made of letters and digits.
fn replace_user(text: &str, user: &str) -> String {
    let re = ci_regex(&regex::escape(user));
    let mut out = String::new();
    let mut last = 0;
    for m in re.find_iter(text) {
        let before = text[..m.start()].chars().next_back();
        let after = text[m.end()..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
        if word(before) || word(after) {
            continue;
        }
        out.push_str(&text[last..m.start()]);
        out.push_str("<user>");
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Strips what could identify the user from an error message or stack: app paths, home, username.
pub fn scrub(text: &str, ctx: &ScrubContext) -> String {
    let mut out = text.to_owned();
    for p in spellings(&ctx.app_path) {
        let trimmed = p.strip_prefix('/').unwrap_or(&p);
        out = ci_regex(&format!(r"file:///?{}[\\/]?", regex::escape(trimmed))).replace_all(&out, "").into_owned();
        out = ci_regex(&format!(r"{}[\\/]?", regex::escape(&p))).replace_all(&out, "").into_owned();
    }
    for p in spellings(&ctx.home) {
        out = ci_regex(&regex::escape(&p)).replace_all(&out, "~").into_owned();
    }
    // A one-letter name would mangle every message; such a name is not much of an identifier anyway.
    if ctx.user.chars().count() >= 2 {
        out = replace_user(&out, &ctx.user);
    }
    out
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_app_paths_relative_and_hides_home_and_username() {
        let mac = ScrubContext {
            home: "/Users/alice".into(),
            user: "alice".into(),
            app_path: "/Applications/Oniko.app/Contents/Resources/app.asar".into(),
        };
        let stack = [
            "Error: boom",
            "    at f (file:///Applications/Oniko.app/Contents/Resources/app.asar/out/renderer/assets/index.js:1:2)",
            "    at g (/Applications/Oniko.app/Contents/Resources/app.asar/out/main/index.js:3:4)",
            "ENOENT: open '/Users/alice/projects/x'",
            "owner alice, not malice",
        ]
        .join("\n");
        let expected = [
            "Error: boom",
            "    at f (out/renderer/assets/index.js:1:2)",
            "    at g (out/main/index.js:3:4)",
            "ENOENT: open '~/projects/x'",
            "owner <user>, not malice",
        ]
        .join("\n");
        assert_eq!(scrub(&stack, &mac), expected);
    }

    #[test]
    fn handles_windows_paths_in_both_slash_styles() {
        let win = ScrubContext {
            home: r"C:\Users\Bob".into(),
            user: "Bob".into(),
            app_path: r"C:\Program Files\Oniko\resources\app.asar".into(),
        };
        assert_eq!(scrub("at file:///C:/Program Files/Oniko/resources/app.asar/out/main/index.js", &win), "at out/main/index.js");
        assert_eq!(scrub("at file:///C:/Program%20Files/Oniko/resources/app.asar/out/a.js", &win), "at out/a.js");
        assert_eq!(scrub(r"C:\Users\Bob\x and C:/Users/Bob/y", &win), r"~\x and ~/y");
    }

    #[test]
    fn leaves_one_letter_usernames_alone() {
        let ctx = ScrubContext { home: "/home/a".into(), user: "a".into(), app_path: "/opt/oniko".into() };
        assert_eq!(scrub("a value", &ctx), "a value");
    }

    #[test]
    fn truncates_with_an_ellipsis_within_the_limit() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
