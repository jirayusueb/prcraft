//! What the AI engine may see: realistic leaks that must be masked, ordinary code that must not be touched, and the files
//! that never leave this Mac. Every value is invented.

use pr_craft_core::redact::{MASK, ReadRules, redact};

const GITHUB: &str = "gho_a1B2c3D4e5a1B2c3D4e5a1B2c3D4e5a1B2c3D4";

/// Each entry is a line as it appears in a diff, and the secret that must not survive it.
fn assert_masked(table: &[(&str, &str)]) {
    for (line, secret) in table {
        let out = redact(line);
        assert!(!out.text.contains(secret), "`{secret}` leaked through `{line}` as `{}`", out.text);
        assert!(out.text.contains(MASK) && out.found >= 1, "nothing was masked in `{line}`");
        assert_eq!(out.text.lines().count(), line.lines().count(), "a line was lost in `{line}`");
    }
}

#[test]
fn secret_names_are_found_anywhere_in_the_identifier() {
    assert_masked(&[
        ("SECRET_KEY = 'django-insecure-9x!k2#vfq8w'", "django-insecure-9x!k2#vfq8w"),
        ("secret_key_base: 4f3c9a1b7d2e8f60aa11", "4f3c9a1b7d2e8f60aa11"),
        ("secret_key_base: \"4f3c9a1b7d2e8f60aa11\"", "4f3c9a1b7d2e8f60aa11"),
        ("secretAccessKey: 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY'", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
        ("private_key = \"MIIEvQIBADANBgkqhkiG9w0BAQEF\"", "MIIEvQIBADANBgkqhkiG9w0BAQEF"),
        ("ENCRYPTION_KEY=0123456789abcdef0123456789abcdef", "0123456789abcdef0123456789abcdef"),
        ("DB_PASS=Sup3rS3cret", "Sup3rS3cret"),
        ("const pwd = \"Sup3rS3cret\"", "Sup3rS3cret"),
        ("let apiKey = \"k9x8c7v6b5n4\";", "k9x8c7v6b5n4"),
        ("X-Api-Key: 9f8e7d6c5b4a", "9f8e7d6c5b4a"),
        ("passphrase: tr0ub4dor&3", "tr0ub4dor&3"),
    ]);
}

#[test]
fn unquoted_values_after_a_colon_or_spaced_equals_are_masked() {
    assert_masked(&[
        ("    password: Sup3rS3cret!", "Sup3rS3cret!"),
        ("  token: 5f4dcc3b5aa765d61d8327deb882cf99", "5f4dcc3b5aa765d61d8327deb882cf99"),
        ("password = hunter2xyz99", "hunter2xyz99"),
        ("aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
        ("client_secret = 8a9b7c6d5e4f3a2b", "8a9b7c6d5e4f3a2b"),
        ("password = \"correct horse battery staple\"", "horse battery"),
        ("password=hunter2xyz", "hunter2xyz"),
        ("DB_PASSWORD=correcthorse", "correcthorse"),
        ("cfg.Password = \"s3cr3t-value\"", "s3cr3t-value"),
        ("{\"client_secret\": \"abcd1234efgh\"}", "abcd1234efgh"),
        ("'passwd' => 'letmein99'", "letmein99"),
        ("https://x.test/cb?token=ab12cd34ef56", "ab12cd34ef56"),
        ("api_key: 'k-12345678'", "k-12345678"),
    ]);
}

#[test]
fn a_private_key_is_masked_however_it_is_written() {
    let go = "const key = \"-----BEGIN RSA PRIVATE KEY-----\\n\" +\n\t\"MIIEowIBAAKCAQEAu1SU1LfVLPHCozMx\\n\" +\n\t\"4lgOEePzNm0tRgeLezV6ffAt0gunVTLw\\n\" +\n\t\"-----END RSA PRIVATE KEY-----\\n\"\nfunc next() {}\n";
    let out = redact(go);
    assert!(!out.text.contains("MIIEowIB") && !out.text.contains("4lgOEeP"), "{}", out.text);
    assert_eq!(out.text.lines().count(), go.lines().count());
    assert!(out.text.ends_with("func next() {}\n"), "code after the key is kept: {}", out.text);

    let pgp = "-----BEGIN PGP PRIVATE KEY BLOCK-----\nVersion: GnuPG v2\n\nlQdGBF8aBcYBEAC0q9Zr\n=aB3d\n-----END PGP PRIVATE KEY BLOCK-----\nafter\n";
    let out = redact(pgp);
    assert!(!out.text.contains("lQdGBF8a") && !out.text.contains("GnuPG") && !out.text.contains("=aB3d"), "{}", out.text);
    assert!(out.text.ends_with("after\n"));

    let raw =
        "x\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nnot base64 here at all!\nmore\n-----END OPENSSH PRIVATE KEY-----\ny\n";
    let out = redact(raw);
    assert!(!out.text.contains("b3Blbn") && !out.text.contains("not base64"), "{}", out.text);
    assert!(out.text.starts_with("x\n") && out.text.ends_with("y\n"));

    let unterminated = redact("-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0B\nrest of file\n");
    assert!(
        !unterminated.text.contains("MIIEvQ") && !unterminated.text.contains("rest of file"),
        "an unterminated block runs to the end of the text"
    );

    let one_line = redact("k = \"-----BEGIN PRIVATE KEY-----\\nMIIEvQIBADANBgkqhkiG9w0B\\n-----END PRIVATE KEY-----\" + suffix");
    assert!(one_line.text.contains("+ suffix") && !one_line.text.contains("MIIE"));

    let public = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0C\n-----END PUBLIC KEY-----\n";
    assert_eq!(redact(public).text, public, "a public key is not a secret");
}

#[test]
fn vendor_tokens_and_credentials_in_urls_are_masked_bare() {
    let github = format!("export GITHUB_TOKEN={GITHUB}");
    let slack = format!("WebClient(\"xoxb-{s}\")", s = "123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx");
    let jwt = "const t = \"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r\";";
    assert_masked(&[
        (&github, "a1B2c3D4e5a1B2c3"),
        ("require('stripe')('sk_live_51H8xAbCdEfGhIjKlMnOp')", "sk_live_51H8xAbCdEfGhIjKlMnOp"),
        (&slack, "AbCdEfGhIjKlMnOpQrStUvWx"),
        ("const k = \"AIzaSyA1b2C3d4E5f6G7h8I9j0KlMnOpQrStUvW\";", "AIzaSyA1b2C3d4E5f6G7h8I9j0KlMnOpQrStUvW"),
        ("SG.AbCdEfGhIjKlMnOpQrSt.WxYzAbCdEfGhIjKlMnOpQrStUvWxYz0123456789", "WxYzAbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
        ("OPENAI = sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
        ("DATABASE_URL=postgres://admin:Sup3rS3cret@db.internal/app", "Sup3rS3cret"),
        ("uri: mongodb+srv://root:Sup3rS3cret@cluster0.example.net", "Sup3rS3cret"),
        ("curl https://ci:Sup3rS3cret@git.example.com/x.git", "Sup3rS3cret"),
        ("Authorization: Basic dXNlcjpTdXBlclNlY3JldDEyMw==", "dXNlcjpTdXBlclNlY3JldDEyMw=="),
        (&("hook = https://hooks.".to_owned() + "slack.com/services/T01234567/B01234567/AbCdEfGhIjKlMnOpQrStUvWx"), "AbCdEfGhIjKlMnOpQrStUvWx"),
        (jwt, "eyJzdWIiOiIxMjM0NTY3ODkwIn0"),
        ("aws_access_key_id = AKIAIOSFODNN7EXAMPLE", "AKIAIOSFODNN7EXAMPLE"),
        ("glpat-qqqqqqqqqqqqqqqqqqqq", "qqqqqqqqqqqqqqqqqqqq"),
        ("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r", "dBjftJeZ4CVPmB92K27uhbUJU1p1r"),
    ]);
}

#[test]
fn only_the_secret_goes_not_the_line_around_it() {
    assert_eq!(redact("password = \"hunter2xyz\" // dev only").text, format!("password = \"{MASK}\" // dev only"));
    assert_eq!(redact("postgres://admin:Sup3rS3cret@db.internal/app").text, format!("postgres://admin:{MASK}@db.internal/app"));
    assert_eq!(
        redact("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r").text,
        format!("Authorization: Bearer {MASK}")
    );
    assert_eq!(redact("a\r\npassword: Sup3rS3cret!\r\nb").text, format!("a\r\npassword: {MASK}\r\nb"), "line endings stay");
}

#[test]
fn ordinary_code_is_left_alone() {
    let lines = [
        "let token = next_token();",
        "password_field.focus()",
        "fn secret_key(&self) -> &str {",
        "if (pass) {",
        "token: Token,",
        "pub token: String,",
        "key: String",
        "secret: string;",
        "password: Option<String>,",
        "token: &'a str,",
        "func (s *Server) Login(ctx context.Context, password string) error {",
        "hash := bcrypt.Hash(password)",
        "if password == \"\" || token == \"\" {",
        "return errors.New(\"password is required\")",
        "token := lexer.Next()",
        "tokenType = \"bearer\"",
        "client := NewClient(password=settings.DB_PASSWORD)",
        "let api_key = std::env::var(\"API_KEY\")?;",
        "password_file = \"/etc/app/pw\"",
        "req.Header.Set(\"Authorization\", \"Bearer \"+token)",
        "type Token struct { Value string }",
        "commit := \"3f786850e387550fdab836ed7e6dc881de23001b\"",
        "key := \"AKIA is a prefix\"",
        "PASSWORD=$DB_PASSWORD",
        "const passwordMinLen = 12",
        "token = request.headers.get(\"x\")",
        "password = form.cleaned_data[\"password\"]",
        "let secret = Secret::new(raw);",
        "password: ${DB_PASSWORD}",
        "password: \"{{ vault_password }}\"",
        "token = self.token",
        "const token = await fetchToken();",
        "tokens.iter().map(|token| token.id)",
        "Basic authentication is required for this route",
        "Bearer authentication is required for this route",
        "access_token = response.json()[\"access_token\"]",
        "bypass = true",
        "the password reset flow sends a token by mail",
    ];
    for line in lines {
        let out = redact(line);
        assert_eq!(out.found, 0, "false positive on `{line}`: `{}`", out.text);
        assert_eq!(out.text, line);
    }
}

#[test]
fn the_text_after_a_keys_end_marker_is_redacted_too() {
    assert_masked(&[
        ("-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0B\n-----END PRIVATE KEY-----\"\"\", password=b\"hunter2xyz\")", "hunter2xyz"),
        (
            "x\n-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0B\n-----END PRIVATE KEY-----`, token = \"ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
            "ghp_aaaaaaaa",
        ),
        ("-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0B\n-----END PRIVATE KEY-----''' ; passphrase = 'hunter2xyz'", "hunter2xyz"),
    ]);
}

#[test]
fn a_lone_mention_of_a_key_marker_does_not_hide_or_change_the_code() {
    for code in [
        "if strings.Contains(s, \"-----BEGIN PRIVATE KEY-----\") {\n    return parse(s)\n}\nfunc next() {}\n",
        "re := regexp.MustCompile(`-----BEGIN [A-Z ]*PRIVATE KEY-----`)\nnext()\n",
        "header, footer := \"-----BEGIN PRIVATE KEY-----\", \"-----END PRIVATE KEY-----\"\nnext()\n",
    ] {
        let out = redact(code);
        assert_eq!(out.text, code, "a mention of the marker is not a key");
        assert_eq!(out.found, 0);
    }
    let inline = redact("key = \"-----BEGIN PRIVATE KEY-----MIIEvQIBADANBgkqhkiG9w0BAQEF\" // dev\n");
    assert!(inline.text.contains("// dev") && !inline.text.contains("MIIEvQ"), "only the body run goes: {}", inline.text);
}

#[test]
fn a_commented_out_key_is_masked_to_its_end() {
    for prefix in ["# ", " * ", "> ", "-- ", " | ", "// ", "/// ", "; "] {
        let text = format!(
            "{prefix}-----BEGIN RSA PRIVATE KEY-----\n{prefix}MIIEowIBAAKCAQEAu1SU1LfVLPHCozMx\n{prefix}4lgOEePzNm0tRgeLezV6ffAt0gunVTLw\n{prefix}-----END RSA PRIVATE KEY-----\nnext()\n"
        );
        let out = redact(&text);
        assert!(!out.text.contains("MIIEowIB") && !out.text.contains("4lgOEeP"), "prefix `{prefix}` leaked: {}", out.text);
        assert!(out.text.ends_with("next()\n"));
    }
    let long = redact("x\n-----BEGIN PRIVATE KEY-----\n?? MIIEowIBAAKCAQEAu1SU1LfVLPHCozMxMIIEowIBAAKCAQEAu1SU1LfVLPHCozMx\ny\n");
    assert!(!long.text.contains("MIIEowIB"), "a base64 run of 40 or more opens the block whatever surrounds it: {}", long.text);
}

#[test]
fn field_names_markup_and_parser_code_are_left_alone() {
    for line in [
        // a quoted secret name followed by another string
        "fields = [\"username\", \"password\", \"email\"]",
        "exclude = [\"password\", \"last_login\", \"date_joined\"]",
        "columns := []string{\"id\", \"email\", \"password\", \"created_at\"}",
        "protected $hidden = ['password', 'remember_token'];",
        "{\"grant_type\": \"password\", \"username\": \"u\"}",
        "{\"token_type\": \"Bearer\", \"expires_in\": 3600}",
        "case \"password\", \"token\": return \"***\"",
        // attributes and member paths
        "<input type=\"password\" name=\"password\" />",
        "<input id=\"password\" type=\"password\" className=\"form-control\" />",
        "<a href=\"/reset-password\" class=\"link\">",
        "<label htmlFor=\"password\" className=\"label\">",
        "session.token.expires = expiresAt;",
        "user.password.len = length;",
        "token.list.offset = offset;",
        "self.pass.buffers[index] = buffer;",
        // arguments of calls on the right-hand side
        "token := strings.TrimPrefix(header, \"Bearer \")",
        "token := strings.TrimSpace(strings.TrimPrefix(auth, \"Bearer\"))",
        // constructors and methods that merely contain the word
        "Token(\"NUMBER\", value)",
        "new Token(\"IDENT\", \"foo\")",
        "lexer.peek_token(\"else\")",
        "lexer.token(\"let\")",
        "Pass(\"lowering\")",
        "pm.add_pass(\"inline\")",
        "manager.register_pass(\"dead-code-elimination\")",
        "Secret(\"db-password\")",
        "api.secret(\"app/db\")",
        "password(\"reset\")",
        "level_token(\"ERROR\")",
        // variables on the right-hand side
        "self.tokens = tokens;",
        "*expr_tokens = attr_tokens;",
        "key->private_key = private_key;",
        "fb_key.raw_pass = raw_pass;",
        "password: hashedPassword,",
        "token: refreshToken,",
        "User{Password: hashedPassword}",
        "let mut tokens = quote! {",
        "pass: base?,",
        "// First pass: validates the container",
        "method(setCredential:forProtectionSpace:)",
        // prose
        "// the token authentication_middleware_handler_v2 checks it",
        "/// Token refresh_interval_seconds_default",
    ] {
        let out = redact(line);
        assert_eq!(out.found, 0, "false positive on `{line}`: `{}`", out.text);
        assert_eq!(out.text, line);
    }
}

#[test]
fn a_secret_word_inside_a_string_does_not_swallow_the_rest_of_the_line() {
    for line in [
        "console.log(\"Token:\", token);",
        "print(\"password:\", password)",
        "fmt.Println(\"token:\", token)",
        "logger.info(\"Token: \" + token)",
        "System.out.println(\"Token: \" + token);",
        "throw new Error(\"Invalid token: \" + token);",
        "input(\"Password: \").strip()",
        "let password = rpassword::prompt_password(\"Password: \")?;",
        "fetch(\"/api/verify?token=\" + token)",
        "url = \"/reset?token=\" + token",
        "body = \"username=\" + user + \"&password=\" + pw",
        "String q = \"SELECT * FROM users WHERE password = '\" + pw + \"'\";",
        "let q = format!(\"SELECT * FROM users WHERE password = '{}'\", pw);",
        "echo \"token: $TOKEN\"",
    ] {
        let out = redact(line);
        assert_eq!(out.found, 0, "false positive on `{line}`: `{}`", out.text);
        assert_eq!(out.text, line);
    }
}

#[test]
fn a_default_after_a_call_or_an_index_is_masked() {
    assert_masked(&[
        ("password = ENV[\"PW\"] || \"hunter2xyz\"", "hunter2xyz"),
        ("password: <%= ENV[\"DB_PASSWORD\"] || \"hunter2xyz\" %>", "hunter2xyz"),
        ("password = os.environ[\"PW\"] or \"hunter2xyz\"", "hunter2xyz"),
        ("password = os.environ.get(\"PW\") or \"hunter2xyz\"", "hunter2xyz"),
        ("password = process.env[\"PW\"] || \"hunter2xyz\"", "hunter2xyz"),
        ("password = process.env.PW ?? 'hunter2xyz'", "hunter2xyz"),
    ]);
    for line in ["password = ENV[\"PW\"] || other", "password = os.environ[\"PW\"]", "token = a or b"] {
        assert_eq!(redact(line).text, line);
    }
}

#[test]
fn a_secret_between_two_lookup_defaults_is_masked_and_the_defaults_after_it_are_not() {
    let lines = [
        "config = { password: ENV.fetch(\"PW\", \"dev\"), secret: \"SECRET1\", host: ENV.fetch(\"H\", \"localhost\") }",
        "client = Client(password=os.getenv(\"PW\", \"devpw\"), client_secret=\"SECRET1\", url=os.getenv(\"URL\", \"http://x\"))",
        "settings = dict(db_password=os.environ.get(\"DB_PW\", \"dev\"), jwt_secret=\"SECRET1\", debug=os.environ.get(\"DEBUG\", \"true\"))",
        "const cfg = { token: config.get(\"token\", \"dev-token\"), secret: \"SECRET1\", url: config.get(\"url\", \"http://localhost\") };",
        "token = os.getenv(\"T\", \"dev\"); secret = \"SECRET1\"; url = os.getenv(\"U\", \"http://localhost\")",
        "password = os.getenv(\"PW\", \"devpw\") and secret = \"SECRET1\" and other(1, \"zzz\")",
    ];
    for line in lines {
        let out = redact(line);
        assert!(!out.text.contains("SECRET1"), "a secret between two defaults leaked through `{line}`: `{}`", out.text);
        for kept in ["localhost", "http://x", "\"true\"", "\"zzz\""] {
            assert_eq!(out.text.contains(kept), line.contains(kept), "`{kept}` is not a secret and must stay in `{}`", out.text);
        }
    }
    for line in ["url=os.getenv(\"URL\", \"http://x\")", "debug=os.environ.get(\"DEBUG\", \"true\")"] {
        assert_eq!(redact(line).text, line);
    }
}

#[test]
fn a_lowercase_pass_counts_only_with_a_quoted_literal() {
    assert_masked(&[("auth: { user: 'me', pass: 'hunter2xyz' }", "hunter2xyz"), ("pass = \"hunter2xyz\"", "hunter2xyz")]);
    for line in ["pass: base?,", "raw_pass = raw_pass", "pass = next_pass", "// First pass: validates the container"] {
        assert_eq!(redact(line).text, line);
    }
}

#[test]
fn a_key_on_one_line_is_masked_through_quotes_pluses_and_escapes() {
    assert_masked(&[
        (
            "const key = \"-----BEGIN RSA PRIVATE KEY-----\\n\" + \"MIIEowIBAAKCAQEAu1SU1LfVLPHCozMx\\n\" + \"-----END RSA PRIVATE KEY-----\"",
            "MIIEowIBAAKCAQEAu1SU1LfVLPHCozMx",
        ),
        (
            "const key = \"-----BEGIN RSA PRIVATE KEY-----\" + \"MIIEowIBAAKCAQEAu1SU1LfVLPHCozMx\" + \"-----END RSA PRIVATE KEY-----\"",
            "MIIEowIBAAKCAQEAu1SU1LfVLPHCozMx",
        ),
        ("pem := \"-----BEGIN PRIVATE KEY-----\\\\nMIIEvQIBADANBgkqhkiG9w0B\\\\n-----END PRIVATE KEY-----\\\\n\"", "MIIEvQIBADANBgkqhkiG9w0B"),
    ]);
    let mention = "header, footer := \"-----BEGIN PRIVATE KEY-----\", \"-----END PRIVATE KEY-----\"";
    assert_eq!(redact(mention).text, mention, "two markers with nothing between them are not a key");
}

#[test]
fn setters_and_pairs_that_do_hold_a_secret_are_still_masked() {
    assert_masked(&[
        ("config.set_password(\"hunter2xyz\")", "hunter2xyz"),
        ("client.withSecret(\"hunter2xyz\")", "hunter2xyz"),
        ("builder.passphrase(\"hunter2xyz\")", "hunter2xyz"),
        ("os.putenv(\"API_TOKEN\", \"hunter2xyz\")", "hunter2xyz"),
        ("DB_PASS=Sup3rS3cret", "Sup3rS3cret"),
    ]);
}

#[test]
fn settings_that_share_a_secret_word_are_left_alone() {
    for line in [
        "max_tokens=1000",
        "response = client.chat(max_tokens=1000, temperature=0)",
        "\"max_tokens\": 4096",
        "MAX_TOKENS = 4096",
        "budget_tokens: 16000",
        "credentials: \"include\"",
        "credentials: \"omit\"",
        "fetch(url, { credentials: \"include\" })",
        "fetch(url, { credentials: 'same-origin' })",
        "String pem = \"-----BEGIN PRIVATE KEY-----\\n\" + privateKeyContent + \"\\n-----END PRIVATE KEY-----\";",
        "// PEM looks like -----BEGIN PRIVATE KEY----- followed by the base64 encoded key -----END PRIVATE KEY-----",
    ] {
        let out = redact(line);
        assert_eq!(out.found, 0, "false positive on `{line}`: `{}`", out.text);
        assert_eq!(out.text, line);
    }
    assert_masked(&[
        ("secrets = [\"hunter2xyz\"]", "hunter2xyz"),
        ("TOKENS=abcdef0123456789", "abcdef0123456789"),
        ("credentials: \"hunter2xyz\"", "hunter2xyz"),
    ]);
}

#[test]
fn a_long_secret_is_masked_whole_however_long_it_is() {
    for len in [1_100, 1_700, 3_200, 70_000] {
        let body = format!("MIIE{}", "AbCd0123+/".repeat(len / 10));
        for line in [
            format!("private_key = \"{body}\""),
            format!("private_key={body}"),
            format!("export PRIVATE_KEY={body}"),
            format!("export PRIVATE_KEY=\"{body}\""),
            format!("password: {body}"),
            format!("<password>{body}</password>"),
        ] {
            let out = redact(&line);
            let (head, tail) = (body.get(..60).unwrap_or_default(), body.get(body.len() - 40..).unwrap_or_default());
            assert!(!out.text.contains(head) && !out.text.contains(tail), "{len} characters leaked through `{}`", line.get(..40).unwrap_or_default());
            assert!(out.text.len() < 200, "the whole value is replaced, not a part of it: {} bytes left", out.text.len());
        }
    }
}

#[test]
fn a_secret_in_a_very_long_line_of_strings_is_masked() {
    // A minified bundle: strings holding commas, blanks and semicolons, and a secret every 20 units.
    let mut line = String::new();
    for unit in 0..30_000 {
        match unit % 2 {
            0 => line.push_str("{s:\"a, b c\"},"),
            _ => line.push_str("x = 1; y = \"a b\";"),
        }
        if unit % 20 == 0 {
            line.push_str(&format!("password = \"hunter2xyz{unit}\";"));
        }
    }
    let out = redact(&line);
    assert!(line.len() > 400_000);
    assert!(!out.text.contains("hunter2xyz"), "a secret leaked through a long line");
    assert!(out.text.contains("{s:\"a, b c\"},x = 1; y = \"a b\";"), "the strings around the secrets are kept");
    assert_eq!(out.found, 1_500);
}

/// Files that quote secret shapes on purpose: the redactor and its tests.
const FIXTURE_FILES: [&str; 3] = ["redact.rs", "ai.rs", "runner.rs"];

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | "node_modules") && !name.starts_with('.') {
                walk(&path, out);
            }
        } else if matches!(path.extension().and_then(|ext| ext.to_str()), Some("rs" | "md")) {
            out.push(path);
        }
    }
}

/// The lever that ends the loop of false positives: the redactor must leave this repository's own code and documents
/// untouched. A rule that mangles ordinary code fails here, whatever shape the reviewer thought of.
#[test]
fn the_redactor_leaves_this_repositorys_own_sources_unchanged() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for folder in ["crates", "apps", "xtask"] {
        walk(&root.join(folder), &mut files);
    }
    if let Ok(entries) = std::fs::read_dir(&root) {
        files.extend(entries.flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "md")));
    }
    assert!(files.len() > 20, "the walk found only {} files", files.len());
    let mut changed = Vec::new();
    for path in &files {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        // The it test itself (`tests/it/redact.rs`, named `redact.rs`) is in the list too: it quotes the shapes on purpose.
        if FIXTURE_FILES.contains(&name.as_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let out = redact(&text);
        for (before, after) in text.lines().zip(out.text.lines()) {
            if before != after {
                changed.push(format!("{}: {before}", path.display()));
            }
        }
    }
    assert!(changed.is_empty(), "the redactor changed ordinary lines:\n{}", changed.join("\n"));
}

#[test]
fn type_annotated_and_prefixed_literals_are_masked_not_the_type() {
    assert_masked(&[
        ("password: str = \"hunter2xyz\"", "hunter2xyz"),
        ("secret_key: str = \"dev-secret-key-123\"", "dev-secret-key-123"),
        ("const password: string = 'hunter2xyz';", "hunter2xyz"),
        ("const DB_PASSWORD: &str = \"hunter2xyz\";", "hunter2xyz"),
        ("static SECRET: &'static str = \"hunter2xyz\";", "hunter2xyz"),
        ("val password: String = \"hunter2xyz\"", "hunter2xyz"),
        ("let apiKey: String = \"hunter2xyz\";", "hunter2xyz"),
        ("var Password string = \"hunter2xyz\"", "hunter2xyz"),
        ("password = b\"hunter2xyz\"", "hunter2xyz"),
        ("password=b'hunter2xyz'", "hunter2xyz"),
        ("password = r'hunter2xyz'", "hunter2xyz"),
        ("SECRET_KEY: str = \"dev-secret-key-123\"", "dev-secret-key-123"),
        ("TOKEN: int = 123456", "123456"),
        ("password: Optional[str] = \"hunter2xyz\"", "hunter2xyz"),
    ]);
    for type_only in ["API_KEY: str", "JWT_SECRET: bool = False", "password: Optional[str] = None", "SECRET_KEY: str = os.environ[\"X\"]"] {
        let out = redact(type_only);
        assert_eq!(out.text, type_only, "the type word is not the secret");
    }
}

#[test]
fn a_secret_given_outside_the_name_equals_value_shape_is_masked() {
    assert_masked(&[
        ("define( 'DB_PASSWORD', 'hunter2xyz' );", "hunter2xyz"),
        ("os.environ[\"DB_PASSWORD\"] = \"hunter2xyz\"", "hunter2xyz"),
        ("config[\"password\"] = \"hunter2xyz\"", "hunter2xyz"),
        ("os.Setenv(\"DB_PASSWORD\", \"hunter2xyz\")", "hunter2xyz"),
        ("props.setProperty(\"password\", \"hunter2xyz\")", "hunter2xyz"),
        ("user.setPassword(\"hunter2xyz\")", "hunter2xyz"),
        ("builder.password(\"hunter2xyz\")", "hunter2xyz"),
        ("<password>hunter2xyz</password>", "hunter2xyz"),
        ("<server><password>hunter2xyz</password></server>", "hunter2xyz"),
        ("password = os.getenv(\"PW\", \"hunter2xyz\")", "hunter2xyz"),
        ("password: process.env.PW || \"hunter2xyz\",", "hunter2xyz"),
        ("passwords = [\"hunter2xyz\"]", "hunter2xyz"),
        ("headers = {\"Authorization\": \"Token 0123456789abcdef0123456789abcdef\"}", "0123456789abcdef0123456789abcdef"),
        ("Authorization: Token 0123456789abcdef0123456789abcdef", "0123456789abcdef0123456789abcdef"),
        ("ENV DB_PASSWORD hunter2xyz", "hunter2xyz"),
    ]);
}

#[test]
fn code_that_only_names_a_secret_is_still_left_alone() {
    for line in [
        "password = os.getenv(\"PW\")",
        "let api_key = std::env::var(\"API_KEY\")?;",
        "token = os.environ.get(\"TOKEN\")",
        "fn password(&self) -> String {",
        "def password(self):",
        "getenv(\"DB_PASSWORD\")",
        "errors.New(\"password is required\")",
        "token: Token = Token::new(raw)",
        "token authentication is required for this route",
        "ENV NODE_ENV production",
        "<token>${TOKEN}</token>",
    ] {
        let out = redact(line);
        assert_eq!(out.found, 0, "false positive on `{line}`: `{}`", out.text);
        assert_eq!(out.text, line);
    }
}

#[test]
fn the_documented_limit_is_a_constant_the_ui_can_show() {
    assert!(pr_craft_core::redact::REDACTION_NOTICE.contains("best effort") && pr_craft_core::redact::REDACTION_NOTICE.contains("never-send"));
}

#[test]
fn never_send_drops_whole_files_by_pattern_at_any_depth() {
    let rules = ReadRules::default();
    for path in [
        ".env",
        "deploy/.env.production",
        "secrets/db.yaml",
        "deploy/secrets/prod.yaml",
        "infra/k8s/secrets/db.yaml",
        "services/api/secrets/prod.yaml",
        "certs/tls.PEM",
        "certs/tls.pem",
        "keys/server.key",
        "id_rsa",
        "home/.ssh/id_rsa.pub",
        ".npmrc",
        "web/.npmrc",
        ".netrc",
        "terraform.tfstate",
        "infra/terraform.tfstate.backup",
        "prod.tfvars",
        "infra/prod.tfvars",
        "certs/client.p12",
        "app/release.keystore",
        "aws/credentials",
    ] {
        assert!(rules.withheld(path), "{path} must never be sent");
    }
    for path in ["src/main.rs", "docs/secretary.md", "src/keyboard.rs", "README.md", "crates/core/src/lib.rs"] {
        assert!(!rules.withheld(path), "{path} is ordinary");
    }
    let own = ReadRules { never_send: vec!["billing/**".into(), "!keep.pem".into()], ..ReadRules::default() };
    assert!(own.withheld("billing/a.go") && own.withheld("Billing/A.go"));
    assert!(!own.withheld("keep.pem"), "a `!` entry never turns into a match-everything rule");
    assert!(!ReadRules { never_send: Vec::new(), ..ReadRules::default() }.withheld("secrets/db.yaml"), "no patterns drops nothing");
}
