use anyhow::{Context, Result, anyhow};
use fs_err as fs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Location of config dir relative to the home directory.
const CONFIG_DIR_NAME: &str = ".qq";
const CONFIG_FILE_NAME: &str = "config.json";
const LOCAL_PROVIDER_PLACEHOLDER_API_KEY: &str = "qqqa-local-placeholder";

#[derive(Debug)]
pub struct InitExistsError {
    pub path: PathBuf,
}

impl std::fmt::Display for InitExistsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Config already exists at {}", self.path.display())
    }
}

impl std::error::Error for InitExistsError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderMode {
    Http,
    Cli,
}

impl Default for ProviderMode {
    fn default() -> Self {
        ProviderMode::Http
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliEngine {
    Codex,
    Claude,
}

impl CliEngine {
    pub fn supports_streaming(self) -> bool {
        matches!(self, CliEngine::Claude)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliProviderConfig {
    pub engine: CliEngine,
    pub binary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_args: Vec<String>,
    /// Optional hard override for the CLI model flag (useful if the vendor retires a default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProvider {
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    pub env_key: String,
    /// Optional inline api key in config. Env var takes precedence only if this is absent.
    #[serde(default)]
    pub api_key: Option<String>,
    /// True when the provider targets a local runtime (no API key required).
    #[serde(default)]
    pub local: bool,
    /// Optional TLS customization per provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<ProviderTlsConfig>,
    /// Whether the provider talks to an HTTP API or a CLI binary.
    #[serde(default)]
    pub mode: ProviderMode,
    /// Optional CLI-specific parameters (required when mode = cli).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderTlsConfig {
    /// Path to a PEM/DER CA bundle. Relative paths are resolved against ~/.qq/.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_bundle_path: Option<PathBuf>,
    /// Optional env var name whose value points to the bundle path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_bundle_env: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedTlsConfig {
    pub ca_bundle_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub model_provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Optional per-profile request timeout override (seconds as string)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_profile: String,
    pub model_providers: HashMap<String, ModelProvider>,
    pub profiles: HashMap<String, Profile>,
    #[serde(default)]
    pub include_history: bool,
    /// Automatically copy the first recommended <cmd> block from qq answers.
    #[serde(default)]
    pub copy_first_command: bool,
    /// Optional flag to control emoji usage in prompts.
    /// If present and not equal to "0" or "false" (case-insensitive), emojis are disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_emoji: Option<String>,
    /// Optional list of additional commands allowed for qa execute_command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_allowlist: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        // Matches schema in PORTING_TO_RUST.md
        let mut model_providers = HashMap::new();
        model_providers.insert(
            "openai".to_string(),
            ModelProvider {
                name: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                env_key: "OPENAI_API_KEY".to_string(),
                api_key: None,
                local: false,
                tls: None,
                mode: ProviderMode::Http,
                cli: None,
            },
        );
        model_providers.insert(
            "openrouter".to_string(),
            ModelProvider {
                name: "OpenRouter".to_string(),
                base_url: "https://openrouter.ai/api/v1".to_string(),
                env_key: "OPENROUTER_API_KEY".to_string(),
                api_key: None,
                local: false,
                tls: None,
                mode: ProviderMode::Http,
                cli: None,
            },
        );
        model_providers.insert(
            "groq".to_string(),
            ModelProvider {
                name: "Groq".to_string(),
                base_url: "https://api.groq.com/openai/v1".to_string(),
                env_key: "GROQ_API_KEY".to_string(),
                api_key: None,
                local: false,
                tls: None,
                mode: ProviderMode::Http,
                cli: None,
            },
        );
        model_providers.insert(
            "anthropic".to_string(),
            ModelProvider {
                name: "Anthropic".to_string(),
                base_url: "https://api.anthropic.com/v1".to_string(),
                env_key: "ANTHROPIC_API_KEY".to_string(),
                api_key: None,
                local: false,
                tls: None,
                mode: ProviderMode::Http,
                cli: None,
            },
        );
        model_providers.insert(
            "ollama".to_string(),
            ModelProvider {
                name: "Ollama".to_string(),
                base_url: "http://127.0.0.1:11434/v1".to_string(),
                env_key: "OLLAMA_API_KEY".to_string(),
                api_key: None,
                local: true,
                tls: None,
                mode: ProviderMode::Http,
                cli: None,
            },
        );
        model_providers.insert(
            "codex".to_string(),
            ModelProvider {
                name: "Codex CLI".to_string(),
                base_url: "cli://codex".to_string(),
                env_key: "CODEX_CLI_API_KEY".to_string(),
                api_key: None,
                local: true,
                tls: None,
                mode: ProviderMode::Cli,
                cli: Some(CliProviderConfig {
                    engine: CliEngine::Codex,
                    binary: "codex".to_string(),
                    base_args: vec!["exec".to_string()],
                    model_override: None,
                }),
            },
        );
        model_providers.insert(
            "claude_cli".to_string(),
            ModelProvider {
                name: "Claude Code CLI".to_string(),
                base_url: "cli://claude".to_string(),
                env_key: "CLAUDE_CLI_API_KEY".to_string(),
                api_key: None,
                local: true,
                tls: None,
                mode: ProviderMode::Cli,
                cli: Some(CliProviderConfig {
                    engine: CliEngine::Claude,
                    binary: "claude".to_string(),
                    base_args: Vec::new(),
                    model_override: None,
                }),
            },
        );
        model_providers.insert(
            "gemini".to_string(),
            ModelProvider {
                name: "Gemini".to_string(),
                base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
                env_key: "GEMINI_API_KEY".to_string(),
                api_key: None,
                local: false,
                tls: None,
                mode: ProviderMode::Http,
                cli: None,
            },
        );

        let mut profiles = HashMap::new();
        profiles.insert(
            "openrouter".to_string(),
            Profile {
                model_provider: "openrouter".to_string(),
                model: "openai/gpt-4.1-nano".to_string(),
                reasoning_effort: None,
                temperature: None,
                timeout: None,
            },
        );
        profiles.insert(
            "openai".to_string(),
            Profile {
                model_provider: "openai".to_string(),
                model: "gpt-5-mini".to_string(),
                reasoning_effort: None,
                temperature: None,
                timeout: None,
            },
        );
        profiles.insert(
            "groq".to_string(),
            Profile {
                model_provider: "groq".to_string(),
                model: "openai/gpt-oss-20b".to_string(),
                reasoning_effort: None,
                temperature: None,
                timeout: None,
            },
        );
        profiles.insert(
            "anthropic".to_string(),
            Profile {
                model_provider: "anthropic".to_string(),
                model: "claude-3-5-sonnet-20241022".to_string(),
                reasoning_effort: None,
                temperature: None,
                timeout: None,
            },
        );
        profiles.insert(
            "ollama".to_string(),
            Profile {
                model_provider: "ollama".to_string(),
                model: "llama3.1".to_string(),
                reasoning_effort: None,
                temperature: None,
                timeout: None,
            },
        );
        profiles.insert(
            "codex".to_string(),
            Profile {
                model_provider: "codex".to_string(),
                model: "gpt-5".to_string(),
                reasoning_effort: Some("minimal".to_string()),
                temperature: None,
                timeout: None,
            },
        );
        profiles.insert(
            "claude_cli".to_string(),
            Profile {
                model_provider: "claude_cli".to_string(),
                model: "claude-haiku-4-5".to_string(),
                reasoning_effort: None,
                temperature: None,
                timeout: None,
            },
        );
        profiles.insert(
            "gemini".to_string(),
            Profile {
                model_provider: "gemini".to_string(),
                model: "gemini-3-flash-preview".to_string(),
                reasoning_effort: None,
                temperature: None,
                timeout: None,
            },
        );

        Self {
            default_profile: "openrouter".to_string(),
            model_providers,
            profiles,
            include_history: false,
            copy_first_command: false,
            no_emoji: None,
            command_allowlist: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpConnection {
    pub base_url: String,
    pub api_key: String,
    pub request_timeout_secs: Option<u64>,
    pub is_local: bool,
    pub headers: HashMap<String, String>,
    pub tls: Option<ResolvedTlsConfig>,
}

#[derive(Debug, Clone)]
pub struct CliConnection {
    pub engine: CliEngine,
    pub binary: String,
    pub base_args: Vec<String>,
    pub request_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum ProviderConnection {
    Http(HttpConnection),
    Cli(CliConnection),
}

/// The resolved configuration for a run: provider, model, and backend connection info.
#[derive(Debug, Clone)]
pub struct EffectiveProfile {
    pub provider_key: String,
    pub model: String,
    pub connection: ProviderConnection,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f32>,
}

impl EffectiveProfile {
    pub fn http(&self) -> Option<&HttpConnection> {
        match &self.connection {
            ProviderConnection::Http(conn) => Some(conn),
            _ => None,
        }
    }

    pub fn http_mut(&mut self) -> Option<&mut HttpConnection> {
        match &mut self.connection {
            ProviderConnection::Http(conn) => Some(conn),
            _ => None,
        }
    }

    pub fn cli(&self) -> Option<&CliConnection> {
        match &self.connection {
            ProviderConnection::Cli(conn) => Some(conn),
            _ => None,
        }
    }
}

impl Config {
    /// Load config from disk or create a default one on first run.
    pub fn load_or_init(debug: bool) -> Result<(Self, PathBuf)> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
        let dir = home.join(CONFIG_DIR_NAME);
        let path = dir.join(CONFIG_FILE_NAME);

        if !dir.exists() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("Creating config dir: {}", dir.display()))?;
            set_permissions_dir(&dir, debug).ok();
        }

        if path.exists() {
            let bytes =
                fs::read(&path).with_context(|| format!("Reading config: {}", path.display()))?;
            let cfg: Config =
                serde_json::from_slice(&bytes).with_context(|| "Parsing config JSON")?;
            set_permissions_file(&path, debug).ok();
            Ok((cfg, path))
        } else {
            let cfg = Config::default();
            let json = serde_json::to_vec_pretty(&cfg).unwrap();
            fs::write(&path, json)
                .with_context(|| format!("Writing default config: {}", path.display()))?;
            set_permissions_file(&path, debug).ok();
            Ok((cfg, path))
        }
    }

    /// Save config back to the given path with pretty JSON and safe permissions.
    pub fn save(&self, path: &Path, debug: bool) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).with_context(|| "Serializing config JSON")?;
        fs::write(path, json).with_context(|| format!("Writing config: {}", path.display()))?;
        set_permissions_file(path, debug).ok();
        Ok(())
    }

    /// Whether the no-emoji flag is effectively enabled.
    pub fn no_emoji_enabled(&self) -> bool {
        match &self.no_emoji {
            None => false,
            Some(v) => {
                let v = v.trim().to_ascii_lowercase();
                !(v.is_empty() || v == "0" || v == "false")
            }
        }
    }

    /// Commands explicitly allowed by the user for qa's execute_command tool.
    pub fn command_allowlist(&self) -> Vec<String> {
        self.command_allowlist.clone().unwrap_or_default()
    }

    /// Whether terminal history should be included in prompts by default.
    pub fn history_enabled(&self) -> bool {
        self.include_history
    }

    /// Persist the history preference toggle.
    pub fn set_history_enabled(&mut self, enabled: bool) {
        self.include_history = enabled;
    }

    /// Whether qq should copy the first <cmd> block to the clipboard by default.
    pub fn copy_first_command_enabled(&self) -> bool {
        self.copy_first_command
    }

    /// Persist the clipboard auto-copy toggle.
    pub fn set_copy_first_command(&mut self, enabled: bool) {
        self.copy_first_command = enabled;
    }

    /// Add a command to the custom allowlist. Returns true if the command was newly inserted.
    pub fn add_command_to_allowlist(&mut self, command: &str) -> bool {
        let entry = self.command_allowlist.get_or_insert_with(Vec::new);
        if entry.iter().any(|c| c == command) {
            false
        } else {
            entry.push(command.to_string());
            entry.sort();
            entry.dedup();
            true
        }
    }

    /// Resolve the effective profile based on optional CLI overrides for profile name and model.
    pub fn resolve_profile(
        &self,
        profile_opt: Option<&str>,
        model_override: Option<&str>,
        config_dir: Option<&Path>,
    ) -> Result<EffectiveProfile> {
        let profile_name = profile_opt.unwrap_or(&self.default_profile);
        let profile = self
            .profiles
            .get(profile_name)
            .ok_or_else(|| anyhow!("Profile '{}' not found in config", profile_name))?;
        let provider_key = &profile.model_provider;
        let provider = self
            .model_providers
            .get(provider_key)
            .ok_or_else(|| anyhow!("Model provider '{}' not found in config", provider_key))?;

        let user_model_override = model_override.map(|m| m.to_string());

        let request_timeout_secs = if let Some(raw) = profile.timeout.as_deref() {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                let secs = trimmed.parse::<u64>().with_context(|| {
                    format!(
                        "Invalid timeout '{}' for profile '{}': expected positive integer seconds",
                        raw, profile_name
                    )
                })?;
                if secs == 0 {
                    return Err(anyhow!(
                        "Invalid timeout '{}' for profile '{}': must be greater than zero",
                        raw,
                        profile_name
                    ));
                }
                Some(secs)
            }
        } else {
            None
        };
        let effective_mode = if provider.mode == ProviderMode::Cli {
            ProviderMode::Cli
        } else if provider.cli.is_some() {
            // Backward compatibility: treating presence of CLI config as a signal.
            ProviderMode::Cli
        } else {
            ProviderMode::Http
        };

        let mut model = user_model_override
            .clone()
            .unwrap_or_else(|| profile.model.clone());
        if user_model_override.is_none() && matches!(effective_mode, ProviderMode::Cli) {
            if let Some(cli) = provider.cli.as_ref() {
                if let Some(override_model) = cli
                    .model_override
                    .as_deref()
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                {
                    model = override_model.to_string();
                }
            }
        }

        let connection = match effective_mode {
            ProviderMode::Http => {
                if provider.base_url.trim().is_empty() {
                    return Err(anyhow!(
                        "Provider '{}' must define a non-empty base_url",
                        provider_key
                    ));
                }
                let headers = provider_default_headers(provider_key);
                // Prefer inline api_key; else env var per env_key. Local providers fall back to a
                // placeholder key so callers can continue to send an Authorization header.
                let api_key = if let Some(k) = provider.api_key.clone() {
                    k
                } else if let Ok(value) = std::env::var(&provider.env_key) {
                    value
                } else if provider.local {
                    LOCAL_PROVIDER_PLACEHOLDER_API_KEY.to_string()
                } else {
                    return Err(anyhow!(
                        "Missing API key: set '{}' env var or add 'api_key' to provider '{}' in config",
                        provider.env_key,
                        provider_key
                    ));
                };
                let tls = provider
                    .tls
                    .as_ref()
                    .map(|cfg| cfg.resolve(config_dir))
                    .transpose()?
                    .flatten();

                ProviderConnection::Http(HttpConnection {
                    base_url: provider.base_url.clone(),
                    api_key,
                    request_timeout_secs,
                    is_local: provider.local,
                    headers,
                    tls,
                })
            }
            ProviderMode::Cli => {
                let cli = provider
                    .cli
                    .as_ref()
                    .ok_or_else(|| anyhow!("Provider '{}' missing cli settings", provider_key))?;
                ProviderConnection::Cli(CliConnection {
                    engine: cli.engine.clone(),
                    binary: cli.binary.clone(),
                    base_args: cli.base_args.clone(),
                    request_timeout_secs,
                })
            }
        };

        Ok(EffectiveProfile {
            provider_key: provider_key.clone(),
            model,
            connection,
            reasoning_effort: profile.reasoning_effort.clone(),
            temperature: profile.temperature,
        })
    }

    /// Interactive initializer that writes a fresh config and allows choosing
    /// the default profile and optionally storing an API key inline.
    /// This will overwrite an existing config file.
    pub fn init_interactive(debug: bool) -> Result<PathBuf> {
        use std::io::{self, Write};

        let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
        let dir = home.join(CONFIG_DIR_NAME);
        let path = dir.join(CONFIG_FILE_NAME);

        if !dir.exists() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("Creating config dir: {}", dir.display()))?;
            set_permissions_dir(&dir, debug).ok();
        }

        if path.exists() {
            return Err(InitExistsError { path: path.clone() }.into());
        }

        let mut cfg = Config::default();

        println!("qqqa init — set up your provider and API key");
        println!("\nChoose default profile:");
        println!("  [1] OpenRouter — openai/gpt-4.1-nano (fast, inexpensive)");
        println!("  [2] Groq  — openai/gpt-oss-20b (fast, cheap)");
        println!("  [3] OpenAI — gpt-5-mini (slower, a bit smarter)");
        println!("  [4] Anthropic — claude-3-5-sonnet-20241022 (Claude by Anthropic)");
        println!("  [5] Ollama — llama3.1 via http://127.0.0.1:11434/v1 (runs locally)");
        println!(
            "  [6] Codex CLI — leverage the installed `codex` binary (uses your ChatGPT subscription)"
        );
        println!(
            "  [7] Claude Code CLI — use the local `claude` binary (Claude desktop / npm package)"
        );
        println!("  [8] Gemini — gemini-3-flash-preview (Google's Gemini via OpenAI-compatible API)");
        print!("Enter 1-8 [1]: ");
        io::stdout().flush().ok();
        let mut choice = String::new();
        io::stdin().read_line(&mut choice).ok();
        let choice = choice.trim();
        match choice {
            "2" | "groq" => cfg.default_profile = "groq".to_string(),
            "3" | "openai" => cfg.default_profile = "openai".to_string(),
            "4" | "anthropic" => cfg.default_profile = "anthropic".to_string(),
            "5" | "ollama" => cfg.default_profile = "ollama".to_string(),
            "6" | "codex" => cfg.default_profile = "codex".to_string(),
            "7" | "claude" | "claude-cli" | "claude_code" => {
                cfg.default_profile = "claude_cli".to_string()
            }
            "8" | "gemini" => cfg.default_profile = "gemini".to_string(),
            "1" | "openrouter" => cfg.default_profile = "openrouter".to_string(),
            _ => cfg.default_profile = "openrouter".to_string(),
        }

        // Ask for API key for the chosen provider (optional).
        let provider_key = cfg.default_profile.clone();
        let provider = cfg
            .model_providers
            .get(&provider_key)
            .ok_or_else(|| anyhow!("Internal error: missing provider {}", provider_key))?
            .clone();

        if provider.mode == ProviderMode::Http {
            let env_hint = provider.env_key.clone();
            if provider.local {
                println!(
                    "\n{} runs locally at {}. No API key is required; press Enter to continue or paste one if your setup needs it.",
                    provider.name, provider.base_url
                );
            } else {
                println!(
                    "\nEnter {} (optional). Leave empty to use env var {}.",
                    provider.name, env_hint
                );
            }
            print!("{}: ", provider.name);
            io::stdout().flush().ok();
            let mut key_in = String::new();
            io::stdin().read_line(&mut key_in).ok();
            let key_in = key_in.trim().to_string();

            if !key_in.is_empty() {
                if let Some(mp) = cfg.model_providers.get_mut(&provider_key) {
                    mp.api_key = Some(key_in);
                }
            } else if !provider.local {
                // No inline key; check if env is present and warn if missing.
                if std::env::var(&env_hint).is_err() {
                    println!(
                        "Hint: export {}=YOUR_KEY (e.g., add to your shell profile).",
                        env_hint
                    );
                }
            }

            if provider.local {
                let default_base = provider.base_url.clone();
                println!(
                    "\n{} runs via an OpenAI-compatible server. Override the base URL if you changed the port (default {}).",
                    provider.name, default_base
                );
                print!("Base URL [{}]: ", default_base);
                io::stdout().flush().ok();
                let mut base_in = String::new();
                io::stdin().read_line(&mut base_in).ok();
                let base_in = base_in.trim();
                if !base_in.is_empty() {
                    if let Some(mp) = cfg.model_providers.get_mut(&provider_key) {
                        mp.base_url = base_in.to_string();
                    }
                }
            }
        } else {
            let (binary, instructions, auth_note) = cfg
                .model_providers
                .get(&provider_key)
                .and_then(|mp| mp.cli.as_ref())
                .map(|cli| cli.binary.clone())
                .map(|bin| {
                    let install = match provider_key.as_str() {
                        "codex" => format!(
                            "Install Codex CLI (ChatGPT desktop or `pip install codex-cli`) and ensure '{}' is on your PATH.",
                            bin
                        ),
                        "claude_cli" => format!(
                            "Install Claude Code (`npm install -g @anthropic-ai/claude-code`) so '{}' is available on your PATH.",
                            bin
                        ),
                        _ => format!(
                            "Ensure the '{}' CLI binary is installed and on your PATH.",
                            bin
                        ),
                    };
                    let auth = match provider_key.as_str() {
                        "codex" => "No API key is required; the Codex CLI handles auth using your ChatGPT subscription.",
                        "claude_cli" => "No API key is required; run `claude login` so the CLI can reuse your Claude subscription.",
                        _ => "Authentication is handled by the CLI runtime itself.",
                    }
                    .to_string();
                    (bin, install, auth)
                })
                .unwrap_or_else(|| {
                    (
                        "codex".to_string(),
                        "Ensure the CLI binary is installed and on your PATH.".to_string(),
                        "Authentication is handled by the CLI runtime itself.".to_string(),
                    )
                });
            println!("\n{} uses the '{}' CLI binary.", provider.name, binary);
            println!("{}", instructions);
            println!("{}", auth_note);
        }

        println!("\nShare recent `qq` / `qa` commands with the model?");
        println!("  Pros: gives follow-up questions more context without copy/paste.");
        println!(
            "  Cons: sends the last 10 `qq`/`qa` commands from your shell history to the LLM."
        );
        println!("You can change this later per run with CLI flags.");
        print!("Upload those commands by default? [y/N]: ");
        io::stdout().flush().ok();
        let mut history_choice = String::new();
        io::stdin().read_line(&mut history_choice).ok();
        let history_choice = history_choice.trim().to_ascii_lowercase();
        let history_enabled = matches!(history_choice.as_str(), "y" | "yes");
        cfg.set_history_enabled(history_enabled);

        println!("\nAuto-copy the first recommended <cmd> block to your clipboard?");
        println!("  Pros: saves a copy step when you trust your local clipboard.");
        println!(
            "  Cons: anything copied becomes visible to other apps that can read the clipboard."
        );
        print!("Enable auto-copy by default? [y/N]: ");
        io::stdout().flush().ok();
        let mut copy_choice = String::new();
        io::stdin().read_line(&mut copy_choice).ok();
        let copy_choice = copy_choice.trim().to_ascii_lowercase();
        let copy_enabled = matches!(copy_choice.as_str(), "y" | "yes");
        cfg.set_copy_first_command(copy_enabled);

        let json = serde_json::to_vec_pretty(&cfg)?;
        fs::write(&path, json).with_context(|| format!("Writing config: {}", path.display()))?;
        set_permissions_file(&path, debug).ok();
        println!(
            "\nWrote {} with default profile '{}'.",
            path.display(),
            cfg.default_profile
        );
        let copy_status = if cfg.copy_first_command_enabled() {
            "enabled"
        } else {
            "disabled"
        };
        println!("Auto-copy first command: {}.", copy_status);
        Ok(path)
    }
}

impl ProviderTlsConfig {
    pub fn resolve(&self, config_dir: Option<&Path>) -> Result<Option<ResolvedTlsConfig>> {
        let env_path = match self.ca_bundle_env.as_deref() {
            Some(key) if key.trim().is_empty() => {
                return Err(anyhow!("TLS config ca_bundle_env cannot be empty"));
            }
            Some(key) => match std::env::var(key) {
                Ok(value) => {
                    if value.trim().is_empty() {
                        return Err(anyhow!(
                            "Environment variable '{}' for TLS CA bundle is empty",
                            key
                        ));
                    }
                    Some(PathBuf::from(value))
                }
                Err(std::env::VarError::NotPresent) => None,
                Err(err) => {
                    return Err(anyhow!(
                        "Failed to read environment variable '{}': {}",
                        key,
                        err
                    ));
                }
            },
            None => None,
        };

        let chosen = env_path.or_else(|| self.ca_bundle_path.clone());
        let Some(path) = chosen else {
            return Ok(None);
        };

        let resolved = if path.is_absolute() || config_dir.is_none() {
            path
        } else {
            config_dir.unwrap().join(path)
        };

        Ok(Some(ResolvedTlsConfig {
            ca_bundle_path: resolved,
        }))
    }
}

fn provider_default_headers(provider_key: &str) -> HashMap<String, String> {
    match provider_key {
        "openrouter" => {
            let mut headers = HashMap::new();
            headers.insert(
                "HTTP-Referer".to_string(),
                "https://github.com/iagooar/qqqa".to_string(),
            );
            headers.insert("X-Title".to_string(), "qqqa".to_string());
            headers
        }
        _ => HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn tls_config_resolves_relative_path_against_config_dir() {
        let dir = tempdir().unwrap();
        let tls = ProviderTlsConfig {
            ca_bundle_path: Some(PathBuf::from("certs/litellm-ca.pem")),
            ca_bundle_env: None,
        };
        let resolved = tls
            .resolve(Some(dir.path()))
            .expect("resolution succeeds")
            .expect("path present");
        assert_eq!(
            resolved.ca_bundle_path,
            dir.path().join("certs/litellm-ca.pem")
        );
    }

    #[test]
    fn tls_config_prefers_env_value_when_present() {
        let key = "QQQA_TEST_TLS_ENV";
        let dir = tempdir().unwrap();
        let env_path = dir.path().join("env-ca.pem");
        unsafe {
            std::env::set_var(key, env_path.to_string_lossy().to_string());
        }
        let tls = ProviderTlsConfig {
            ca_bundle_path: Some(PathBuf::from("ignored.pem")),
            ca_bundle_env: Some(key.to_string()),
        };
        let resolved = tls
            .resolve(None)
            .expect("resolution succeeds")
            .expect("env value present");
        assert_eq!(resolved.ca_bundle_path, env_path);
        unsafe {
            std::env::remove_var(key);
        }
    }
}

fn set_permissions_dir(path: &Path, _debug: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn set_permissions_file(path: &Path, _debug: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}
