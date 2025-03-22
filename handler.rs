use std::process::{Command, Stdio};
use std::fs::{File, read_to_string};
use std::io::{BufRead, BufReader};
use std::path::Path;
use nix::unistd::{fork, ForkResult, setpgid, Pid};
use nix::sys::wait::waitpid;
use std::collections::HashMap;
use reqwest::blocking::get;
use std::time::{SystemTime, UNIX_EPOCH};

fn mock_llm_decision(program: &str) -> PoliteConfig {
  println!("LLM deciding for {}...", program);
  PoliteConfig {
    niceness: if program.contains("boinc") {5} else {0},
    oom_score_adj: 100
  }
}

#[derive(Debug, Clone)]
struct PoliteConfig {
  niceness: i32,
  oom_score_adj: i32
}

fn parse_config_line(line: &str) -> Result<(i8, PoliteConfig), String> {
  let parts: Vec<&str> = line.split(';').collect();
  if parts.len() < 3 {return Err("Invalid config".to_string())}
  let alias: i8 = parts[0].parse().map_err(|e| e.to_string())?;
  if alias == 0 {return Err("Alias 0 reserved".to_string())}
  let niceness: i32 = parts[1].parse().map_err(|e| e.to_string())?;
  let oom_score_adj: i32 = parts[2].parse().map_err(|e| e.to_string())?;
  if niceness < -20 || niceness > 19 || oom_score_adj < -1000 || oom_score_adj > 1000 {
    return Err("Value out of range".to_string())
  }
  Ok((alias, PoliteConfig {niceness, oom_score_adj}))
}

fn load_local_config(file_path: &str) -> Result<HashMap<i8, PoliteConfig>, String> {
  let file = File::open(file_path).map_err(|e| e.to_string())?;
  let reader = BufReader::new(file);
  let mut configs = HashMap::new();
  let mut in_section = false;
  for line in reader.lines() {
    let line = line.map_err(|e| e.to_string())?.trim().to_string();
    if line == "-START-" {in_section = true; continue}
    if line == "-END-" {break}
    if in_section && !line.is_empty() && !line.starts_with('#') {
      let (alias, config) = parse_config_line(&line)?;
      configs.insert(alias, config);
    }
  }
  Ok(configs)
}

fn fetch_online_config() -> Result<HashMap<i8, PoliteConfig>, String> {
  let url = "https://raw.githubusercontent.com/username/repo/main/polite_config.csv";
  let text = get(url).map_err(|e| format!("Fetch error: {}", e))?.text().map_err(|e| e.to_string())?;
  let mut configs = HashMap::new();
  for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
    if let Ok((alias, config)) = parse_config_line(line) {
      configs.insert(alias, config);
    }
  }
  if configs.is_empty() {Err("No valid online configs".to_string())} else {Ok(configs)}
}

fn apply_runtime_settings(pid: Pid, config: &PoliteConfig) -> Result<(), String> {
  nix::unistd::setpriority(nix::unistd::Priority::Process(pid.into()), config.niceness)
    .map_err(|e| format!("Niceness error: {}", e))?;
  std::fs::write(format!("/proc/{}/oom_score_adj", pid), config.oom_score_adj.to_string())
    .map_err(|e| format!("OOM error: {}", e))?;
  Ok(())
}

fn get_applied_settings(pid: Pid) -> Result<String, String> {
  let nice = nix::unistd::getpriority(nix::unistd::Priority::Process(pid.into()))
    .map_err(|e| format!("Get nice error: {}", e))?;
  let oom = read_to_string(format!("/proc/{}/oom_score_adj", pid))
    .map_err(|e| format!("Get oom error: {}", e))?.trim().to_string();
  Ok(format!("PID {}: niceness={}, oom_score_adj={}", pid, nice, oom))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<String> = std::env::args().collect();
  if args.len() < 2 {
    eprintln!("Usage: polite <command> [args]");
    eprintln!("Commands: run <alias> <program>, status <pid>, list");
    std::process::exit(1);
  }
  let command = &args[1];
  match command.as_str() {
    "run" => {
      if args.len() != 4 {eprintln!("Usage: polite run <alias> <program>"); std::process::exit(1);}
      let alias: i8 = args[2].parse()?;
      let program = &args[3];
      let local_config_file = "polite.conf";
      let config = if alias == 0 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let last_fetch = 0; // Replace with persistent storage
        if now - last_fetch > 3600 {
          match fetch_online_config() {
            Ok(online_configs) => online_configs.get(&65).cloned().unwrap_or_else(|| mock_llm_decision(program)),
            Err(_) => mock_llm_decision(program)
          }
        } else {
          mock_llm_decision(program)
        }
      } else {
        let local_configs = load_local_config(local_config_file)?;
        local_configs.get(&alias).cloned().ok_or_else(|| format!("Alias {} not found", alias))?
      };
      if !Path::new(program).exists() {return Err(format!("Program {} not found", program).into())}
      unsafe {
        match fork()? {
          ForkResult::Parent { child } => {
            apply_runtime_settings(child, &config)?;
            println!("Started {} with alias {}", program, alias);
            waitpid(child, None)?;
          }
          ForkResult::Child => {
            setpgid(0, 0)?;
            Command::new(program).stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).exec();
            std::process::exit(1);
          }
        }
      }
    }
    "status" => {
      if args.len() != 3 {eprintln!("Usage: polite status <pid>"); std::process::exit(1);}
      let pid: Pid = Pid::from_raw(args[2].parse()?);
      println!("{}", get_applied_settings(pid)?);
    }
    "list" => {
      let local_configs = load_local_config("polite.conf")?;
      for (alias, config) in local_configs {
        println!("Alias {}: niceness={}, oom_score_adj={}", alias, config.niceness, config.oom_score_adj);
      }
    }
    _ => eprintln!("Unknown command: {}", command)
  }
  Ok(())
  }
