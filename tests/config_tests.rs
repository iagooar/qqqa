use assert_cmd::cargo::cargo_bin_cmd;
use fs_err as fs;
use qqqa::config::{Config, ProviderConnection, config_dir_for_home};
use serial_test::serial;
use std::path::Path;
use tempfile::tempdir;

fn run_init_with_bin(bin: &str, input: &str) -> Config {
    let home = tempdir().expect("temp dir");
    let home_path = home.path().to_path_buf();
    let mut cmd = match bin {
        "qq" => cargo_bin_cmd!("qq"),
        "qa" => cargo_bin_cmd!("qa"),
        other => panic!("Unsupported bin '{}'", other),
    };
    cmd.arg("--init")
        .env("HOME", &home_path)
        // Remove env vars so prompts behave consistently in tests.
        .env_remove("GROQ_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OLLAMA_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .write_stdin(input);
    cmd.assert().success();

    let config_path = config_dir_for_home(&home_path).join("config.json");
    Config::load_from(&config_path).expect("config json")
}

fn run_qq_init(input: &str) -> Config {
    run_init_with_bin("qq", input)
}

fn run_qa_init(input: &str) -> Config {
    run_init_with_bin("qa", input)
}

fn read_config_from_home(home: &Path) -> Config {
    let config_path = config_dir_for_home(home).join("config.json");
    Config::load_from(&config_path).expect("config json")
}

#[test]
#[serial]
fn local_provider_uses_placeholder_api_key_when_env_missing() {
    unsafe {
        std::env::remove_var("OLLAMA_API_KEY");
    }
    let cfg = Config::default();
    let eff = cfg
        .resolve_profile(Some("ollama"), None, None)
        .expect("ollama profile should resolve");
    match eff.connection {
        ProviderConnection::Http(ref conn) => {
            assert_eq!(conn.api_key, "qqqa-local-placeholder");
            assert!(conn.is_local);
        }
        _ => panic!("ollama should resolve to HTTP connection"),
    }
}

#[test]
#[serial]
fn codex_profile_resolves_to_cli_backend() {
    let cfg = Config::default();
    let eff = cfg
        .resolve_profile(Some("codex"), None, None)
        .expect("codex profile should resolve");
    match eff.connection {
        ProviderConnection::Cli(ref conn) => {
            assert_eq!(conn.binary, "codex");
            assert_eq!(conn.base_args, vec!["exec".to_string()]);
        }
        _ => panic!("codex profile should resolve to CLI backend"),
    }
}

#[test]
#[serial]
fn claude_cli_profile_resolves_to_cli_backend() {
    let cfg = Config::default();
    let eff = cfg
        .resolve_profile(Some("claude_cli"), None, None)
        .expect("claude_cli profile should resolve");
    match eff.connection {
        ProviderConnection::Cli(ref conn) => {
            assert_eq!(conn.binary, "claude");
            assert!(conn.base_args.is_empty());
        }
        _ => panic!("claude_cli profile should resolve to CLI backend"),
    }
}

#[test]
#[serial]
fn gemini_profile_resolves_to_http_backend() {
    unsafe {
        std::env::set_var("GEMINI_API_KEY", "test-gemini-key");
    }
    let cfg = Config::default();
    let eff = cfg
        .resolve_profile(Some("gemini"), None, None)
        .expect("gemini profile should resolve");
    match eff.connection {
        ProviderConnection::Http(ref conn) => {
            assert!(conn.base_url.contains("generativelanguage.googleapis.com"));
            assert_eq!(conn.api_key, "test-gemini-key");
            assert!(!conn.is_local);
        }
        _ => panic!("gemini should resolve to HTTP connection"),
    }
    unsafe {
        std::env::remove_var("GEMINI_API_KEY");
    }
}

#[test]
fn cli_model_override_forces_model_when_set() {
    let mut cfg = Config::default();
    if let Some(provider) = cfg.model_providers.get_mut("claude_cli")
        && let Some(cli) = provider.cli.as_mut()
    {
        cli.model_override = Some("claude-custom".to_string());
    }

    let eff = cfg
        .resolve_profile(Some("claude_cli"), None, None)
        .expect("profile resolves");
    assert_eq!(eff.model, "claude-custom");

    let eff_flag = cfg
        .resolve_profile(Some("claude_cli"), Some("user-pinned"), None)
        .expect("profile resolves with flag");
    assert_eq!(eff_flag.model, "user-pinned");
}

#[test]
fn cli_mode_inferred_from_cli_block_when_mode_missing() {
    let json = r#"{
        "default_profile": "codex",
        "model_providers": {
            "codex": {
                "name": "Codex CLI",
                "env_key": "CODEX_CLI_API_KEY",
                "local": true,
                "cli": {
                    "engine": "codex",
                    "binary": "codex",
                    "base_args": ["exec"]
                }
            }
        },
        "profiles": {
            "codex": {
                "model_provider": "codex",
                "model": "gpt-5"
            }
        }
    }"#;
    let cfg: Config = serde_json::from_str(json).expect("config parses");
    let eff = cfg
        .resolve_profile(Some("codex"), None, None)
        .expect("profile resolves");
    match eff.connection {
        ProviderConnection::Cli(ref conn) => assert_eq!(conn.binary, "codex"),
        _ => panic!("expected CLI provider"),
    }
}

#[test]
fn cli_mode_overrides_base_url_requirement() {
    let json = r#"{
        "default_profile": "codex",
        "model_providers": {
            "codex": {
                "name": "Codex CLI",
                "base_url": "https://example.com/ignored",
                "env_key": "CODEX_CLI_API_KEY",
                "local": true,
                "mode": "cli",
                "cli": {
                    "engine": "codex",
                    "binary": "codex",
                    "base_args": ["exec"]
                }
            }
        },
        "profiles": {
            "codex": {
                "model_provider": "codex",
                "model": "gpt-5"
            }
        }
    }"#;
    let cfg: Config = serde_json::from_str(json).expect("config parses");
    let eff = cfg
        .resolve_profile(Some("codex"), None, None)
        .expect("profile resolves");
    match eff.connection {
        ProviderConnection::Cli(_) => {}
        _ => panic!("expected CLI provider even with base_url present"),
    }
}

#[test]
fn cli_mode_does_not_require_base_url() {
    let json = r#"{
        "default_profile": "codex",
        "model_providers": {
            "codex": {
                "name": "Codex CLI",
                "base_url": "",
                "env_key": "CODEX_CLI_API_KEY",
                "local": true,
                "mode": "cli",
                "cli": {
                    "engine": "codex",
                    "binary": "codex",
                    "base_args": ["exec"]
                }
            }
        },
        "profiles": {
            "codex": {
                "model_provider": "codex",
                "model": "gpt-5"
            }
        }
    }"#;
    let cfg: Config = serde_json::from_str(json).expect("config parses");
    let eff = cfg
        .resolve_profile(Some("codex"), None, None)
        .expect("profile resolves");
    match eff.connection {
        ProviderConnection::Cli(ref conn) => assert_eq!(conn.binary, "codex"),
        _ => panic!("expected CLI provider even without base_url"),
    }
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn init_defaults_to_openrouter_profile_and_keeps_history_off() {
    let cfg = run_qq_init("\n\nn\n");
    assert_eq!(cfg.default_profile, "openrouter");
    assert!(!cfg.history_enabled());
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn init_allows_custom_local_base_url() {
    let custom_base = "http://127.0.0.1:1234/v1";
    let input = format!("5\n\n{}\nn\n", custom_base);
    let cfg = run_qq_init(&input);
    assert_eq!(cfg.default_profile, "ollama");
    let provider = cfg
        .model_providers
        .get("ollama")
        .expect("ollama provider present");
    assert_eq!(provider.base_url, custom_base);
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn qa_init_defaults_to_openrouter_profile() {
    let cfg = run_qa_init("\n\nn\n");
    assert_eq!(cfg.default_profile, "openrouter");
    assert!(!cfg.history_enabled());
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn qa_init_can_select_openai_without_affecting_others() {
    let cfg = run_qa_init("3\n\nn\n");
    assert_eq!(cfg.default_profile, "openai");
    let defaults = Config::default();
    assert_eq!(
        cfg.model_providers.get("groq").map(|p| &p.base_url),
        defaults.model_providers.get("groq").map(|p| &p.base_url)
    );
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn ollama_init_does_not_mutate_remote_providers() {
    let custom_base = "http://127.0.0.1:4321/v1";
    let input = format!("5\n\n{}\nn\n", custom_base);
    let cfg = run_qa_init(&input);
    let defaults = Config::default();
    assert_eq!(
        cfg.model_providers.get("groq").map(|p| &p.base_url),
        defaults.model_providers.get("groq").map(|p| &p.base_url)
    );
    assert_eq!(
        cfg.model_providers.get("openai").map(|p| &p.base_url),
        defaults.model_providers.get("openai").map(|p| &p.base_url)
    );
    assert_eq!(
        cfg.model_providers
            .get("ollama")
            .map(|p| &p.base_url)
            .map(|s| s.as_str()),
        Some(custom_base)
    );
}

#[test]
#[serial]
fn config_copy_first_command_defaults_false_and_is_toggleable() {
    let mut cfg = Config::default();
    assert!(!cfg.copy_first_command_enabled());
    cfg.set_copy_first_command(true);
    assert!(cfg.copy_first_command_enabled());
    cfg.set_copy_first_command(false);
    assert!(!cfg.copy_first_command_enabled());
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn qq_init_can_enable_auto_copy_flag() {
    // Select default profile, skip API key, decline history, enable auto-copy.
    let cfg = run_qq_init("\n\nn\ny\n");
    assert!(!cfg.history_enabled());
    assert!(cfg.copy_first_command_enabled());
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn qq_enable_auto_copy_flag_persists_without_question() {
    let home = tempdir().expect("temp dir");
    let home_path = home.path().to_path_buf();
    let mut cmd = cargo_bin_cmd!("qq");
    cmd.arg("--enable-auto-copy")
        .env("HOME", &home_path)
        .env_remove("GROQ_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OLLAMA_API_KEY")
        .env_remove("GEMINI_API_KEY");
    cmd.assert().success();

    let cfg = read_config_from_home(&home_path);
    assert!(cfg.copy_first_command_enabled());
}

#[test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "dirs::home_dir ignores HOME on Windows, so the temp home is not used"
)]
fn qq_disable_auto_copy_flag_persists_without_question() {
    let home = tempdir().expect("temp dir");
    let home_path = home.path().to_path_buf();

    let mut enable = cargo_bin_cmd!("qq");
    enable
        .arg("--enable-auto-copy")
        .env("HOME", &home_path)
        .env_remove("GROQ_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OLLAMA_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .assert()
        .success();

    let mut disable = cargo_bin_cmd!("qq");
    disable
        .arg("--disable-auto-copy")
        .env("HOME", &home_path)
        .env_remove("GROQ_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OLLAMA_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .assert()
        .success();

    let cfg = read_config_from_home(&home_path);
    assert!(!cfg.copy_first_command_enabled());
}

#[test]
fn gemini_cli_profile_resolves_to_cli_backend() {
    let cfg = Config::default();
    let eff = cfg
        .resolve_profile(Some("gemini_cli"), None, None)
        .expect("gemini_cli profile should resolve");
    match eff.connection {
        ProviderConnection::Cli(ref conn) => assert_eq!(conn.binary, "gemini"),
        _ => panic!("gemini_cli profile should resolve to CLI backend"),
    }
}

#[test]
fn profile_prompt_suffix_overrides_global_suffix() {
    let mut cfg = Config {
        prompt_suffix: Some("global suffix".to_string()),
        ..Default::default()
    };
    cfg.profiles
        .get_mut("openrouter")
        .expect("openrouter profile")
        .prompt_suffix = Some("profile suffix".to_string());
    cfg.model_providers.get_mut("openrouter").unwrap().api_key = Some("test".into());
    let eff = cfg
        .resolve_profile(Some("openrouter"), None, None)
        .expect("profile resolves");
    assert_eq!(eff.prompt_suffix.as_deref(), Some("profile suffix"));
}

#[test]
fn global_prompt_suffix_applies_when_profile_has_none() {
    let mut cfg = Config {
        prompt_suffix: Some("global suffix".to_string()),
        ..Default::default()
    };
    cfg.model_providers.get_mut("openrouter").unwrap().api_key = Some("test".into());
    let eff = cfg
        .resolve_profile(Some("openrouter"), None, None)
        .expect("profile resolves");
    assert_eq!(eff.prompt_suffix.as_deref(), Some("global suffix"));
}

#[test]
#[serial]
fn config_dir_uses_xdg_when_legacy_missing() {
    let home = tempdir().expect("temp home");
    let xdg = tempdir().expect("temp xdg");
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", xdg.path());
    }
    let dir = config_dir_for_home(home.path());
    assert_eq!(dir, xdg.path().join("qq"));
    unsafe {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}

#[test]
#[serial]
fn config_dir_keeps_legacy_when_config_exists() {
    let home = tempdir().expect("temp home");
    let xdg = tempdir().expect("temp xdg");
    let legacy = home.path().join(".qq");
    fs::create_dir_all(&legacy).expect("legacy dir");
    fs::write(legacy.join("config.json"), "{}").expect("legacy config");
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", xdg.path());
    }
    let dir = config_dir_for_home(home.path());
    assert_eq!(dir, legacy);
    unsafe {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}

fn write_json(dir: &Path, value: &serde_json::Value) -> std::path::PathBuf {
    let path = dir.join("config.json");
    fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    path
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn minimal_config_file_layers_over_defaults() {
    let dir = tempdir().unwrap();
    let path = write_json(dir.path(), &serde_json::json!({"default_profile": "groq"}));
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(cfg.default_profile, "groq");
    assert!(cfg.profiles.contains_key("gemini"));
    assert!(cfg.model_providers.contains_key("claude_cli"));
}

#[test]
fn config_from_an_older_release_gets_new_profiles() {
    let dir = tempdir().unwrap();
    let mut old = serde_json::to_value(Config::default()).unwrap();
    for key in ["gemini", "gemini_cli"] {
        old["profiles"].as_object_mut().unwrap().remove(key);
        old["model_providers"].as_object_mut().unwrap().remove(key);
    }
    let path = write_json(dir.path(), &old);
    let cfg = Config::load_from(&path).unwrap();
    assert!(cfg.profiles.contains_key("gemini"));
    assert!(cfg.model_providers.contains_key("gemini_cli"));
}

#[test]
fn nested_override_keeps_the_other_defaults() {
    let dir = tempdir().unwrap();
    let path = write_json(
        dir.path(),
        &serde_json::json!({"profiles": {"openai": {"model": "gpt-x"}}}),
    );
    let cfg = Config::load_from(&path).unwrap();
    let openai = &cfg.profiles["openai"];
    assert_eq!(openai.model, "gpt-x");
    assert_eq!(openai.model_provider, "openai");
    assert_eq!(cfg.default_profile, "openrouter");
}

#[test]
fn save_writes_only_what_differs_from_defaults() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.set_copy_first_command(true);
    cfg.profiles.get_mut("openai").unwrap().model = "gpt-x".into();
    cfg.save(&path, false).unwrap();
    assert_eq!(
        read_json(&path),
        serde_json::json!({
            "default_profile": "openrouter",
            "copy_first_command": true,
            "profiles": {"openai": {"model": "gpt-x"}}
        })
    );
    cfg.set_copy_first_command(false);
    cfg.save(&path, false).unwrap();
    assert!(read_json(&path).get("copy_first_command").is_none());
    let back = Config::load_from(&path).unwrap();
    assert_eq!(back.profiles["openai"].model, "gpt-x");
}

#[test]
fn saving_a_full_old_config_keeps_only_overrides() {
    let dir = tempdir().unwrap();
    let mut old = serde_json::to_value(Config::default()).unwrap();
    old["include_history"] = serde_json::json!(true);
    let path = write_json(dir.path(), &old);
    let cfg = Config::load_from(&path).unwrap();
    cfg.save(&path, false).unwrap();
    assert_eq!(
        read_json(&path),
        serde_json::json!({"default_profile": "openrouter", "include_history": true})
    );
}
