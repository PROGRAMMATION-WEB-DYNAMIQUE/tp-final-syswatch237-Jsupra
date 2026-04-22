use chrono::Local;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use sysinfo::System;

// --- Types métier ---

#[derive(Debug)]
enum SyswatchError {
    CpuDetectionFailed,
}

impl fmt::Display for SyswatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyswatchError::CpuDetectionFailed => write!(f, "Impossible de détecter le CPU"),
        }
    }
}

impl std::error::Error for SyswatchError {}

#[derive(Debug, Clone)]
struct CpuInfo {
    usage_percent: f32,
    core_count: usize,
}

#[derive(Debug, Clone)]
struct MemInfo {
    total_mb: u64,
    used_mb: u64,
    free_mb: u64,
}

#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    name: String,
    cpu_usage: f32,
    memory_mb: u64,
}

#[derive(Debug, Clone)]
struct SystemSnapshot {
    timestamp: String,
    cpu: CpuInfo,
    memory: MemInfo,
    top_processes: Vec<ProcessInfo>,
}

// --- Trait Display ---

impl fmt::Display for CpuInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CPU: {:.1}% ({} cœurs)", self.usage_percent, self.core_count)
    }
}

impl fmt::Display for MemInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MEM: {}MB utilisés / {}MB total ({} MB libres)",
            self.used_mb, self.total_mb, self.free_mb
        )
    }
}

impl fmt::Display for ProcessInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "  [{:>6}] {:<25} CPU:{:>5.1}%  MEM:{:>5}MB",
            self.pid, self.name, self.cpu_usage, self.memory_mb
        )
    }
}

impl fmt::Display for SystemSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "=== SysWatch — {} ===", self.timestamp)?;
        writeln!(f, "{}", self.cpu)?;
        writeln!(f, "{}", self.memory)?;
        writeln!(f, "--- Top Processus ---")?;
        for p in &self.top_processes {
            writeln!(f, "{}", p)?;
        }
        write!(f, "=====================")
    }
}

// --- Collecte des données réelles ---

fn collect_snapshot() -> Result<SystemSnapshot, SyswatchError> {
    let mut sys = System::new_all();

    // Première mesure, puis on attend 1s pour avoir un usage CPU réaliste
    sys.refresh_all();
    thread::sleep(Duration::from_secs(1));
    sys.refresh_all();

    // CPU
    let cpu_usage = sys.global_cpu_usage();
    let core_count = sys.cpus().len();

    if core_count == 0 {
        return Err(SyswatchError::CpuDetectionFailed);
    }

    // Mémoire (sysinfo retourne des octets)
    let total_mb = sys.total_memory() / 1024 / 1024;
    let used_mb  = sys.used_memory()  / 1024 / 1024;
    let free_mb  = sys.free_memory()  / 1024 / 1024;

    // Processus : on récupère les 10 plus gourmands en CPU
    let mut processes: Vec<ProcessInfo> = sys
        .processes()
        .iter()
        .map(|(pid, proc)| ProcessInfo {
            pid: pid.as_u32(),
            name: proc.name().to_string_lossy().to_string(),
            cpu_usage: proc.cpu_usage(),
            memory_mb: proc.memory() / 1024 / 1024,
        })
        .collect();

    // Tri par usage CPU décroissant
    processes.sort_by(|a, b| b.cpu_usage.partial_cmp(&a.cpu_usage).unwrap_or(std::cmp::Ordering::Equal));
    processes.truncate(5);

    Ok(SystemSnapshot {
        timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        cpu: CpuInfo { usage_percent: cpu_usage, core_count },
        memory: MemInfo { total_mb, used_mb, free_mb },
        top_processes: processes,
    })
}

// --- Formatage ---

fn create_ascii_bar(percent: f32) -> String {
    let bar_length = 20;
    let filled = ((percent / 100.0) * bar_length as f32).round() as usize;
    let filled = filled.min(bar_length);
    let empty = bar_length - filled;
    format!("[{}{}]", "█".repeat(filled), "-".repeat(empty))
}

fn format_response(snapshot: &SystemSnapshot, command: &str) -> String {
    match command.trim() {
        "cpu" => {
            format!("{} {}", snapshot.cpu, create_ascii_bar(snapshot.cpu.usage_percent))
        }
        "mem" => {
            let mem_percent = (snapshot.memory.used_mb as f32 / snapshot.memory.total_mb as f32) * 100.0;
            format!("{} {}", snapshot.memory, create_ascii_bar(mem_percent))
        }
        "ps" => {
            let process_lines: Vec<String> = snapshot.top_processes.iter()
                .map(|p| format!("{}", p))
                .collect();
            format!("--- Top Processus ---\n{}", process_lines.join("\n"))
        }
        "all" => {
            let mem_percent = (snapshot.memory.used_mb as f32 / snapshot.memory.total_mb as f32) * 100.0;
            let mut res = format!("=== SysWatch — {} ===\n", snapshot.timestamp);
            res.push_str(&format!("{} {}\n", snapshot.cpu, create_ascii_bar(snapshot.cpu.usage_percent)));
            res.push_str(&format!("{} {}\n", snapshot.memory, create_ascii_bar(mem_percent)));
            res.push_str("--- Top Processus ---\n");
            let process_lines: Vec<String> = snapshot.top_processes.iter()
                .map(|p| format!("{}", p))
                .collect();
            res.push_str(&process_lines.join("\n"));
            res
        }
        "help" => "Commandes disponibles : cpu, mem, ps, all, help, quit".to_string(),
        "quit" => "Au revoir !".to_string(),
        _ => "Commande inconnue. Tapez 'help' pour voir la liste.".to_string(),
    }
}

// --- Logging ---

fn log_action(message: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("syswatch.log") {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(file, "[{}] {}", timestamp, message);
    }
}

// --- Serveur TCP ---

fn handle_client(mut stream: TcpStream, shared_data: Arc<Mutex<SystemSnapshot>>, peer_addr: String) {
    let mut buffer = [0; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(size) => {
                if size == 0 {
                    log_action(&format!("Déconnexion de {}", peer_addr));
                    break;
                }
                let command = String::from_utf8_lossy(&buffer[..size]).trim().to_string();
                if command.is_empty() {
                    continue;
                }

                log_action(&format!("Commande reçue de {} : {}", peer_addr, command));

                let response = {
                    let snapshot = shared_data.lock().unwrap();
                    format_response(&snapshot, &command)
                };

                if let Err(_) = stream.write_all(format!("{}\n", response).as_bytes()) {
                    log_action(&format!("Erreur d'écriture vers {}", peer_addr));
                    break; // Client déconnecté
                }

                if command == "quit" {
                    log_action(&format!("Déconnexion de {}", peer_addr));
                    break;
                }
            }
            Err(_) => {
                log_action(&format!("Erreur de lecture pour {}", peer_addr));
                break;
            }
        }
    }
}

// --- Main ---

fn main() {
    println!("Démarrage de la collecte système...");
    let initial_snapshot = match collect_snapshot() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Erreur critique au démarrage : {}", e);
            return;
        }
    };

    let shared_data = Arc::new(Mutex::new(initial_snapshot));
    let shared_data_updater = Arc::clone(&shared_data);

    // Thread de mise à jour toutes les 5 secondes
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(5));
            if let Ok(new_snapshot) = collect_snapshot() {
                let mut data = shared_data_updater.lock().unwrap();
                *data = new_snapshot;
            }
        }
    });

    let listener = TcpListener::bind("0.0.0.0:7878").expect("Impossible d'écouter sur le port 7878");
    println!("Serveur SysWatch démarré sur le port 7878.");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let peer_addr = stream.peer_addr().map(|addr| addr.to_string()).unwrap_or_else(|_| "Inconnu".to_string());
                log_action(&format!("Nouvelle connexion de {}", peer_addr));

                let shared_data_client = Arc::clone(&shared_data);
                thread::spawn(move || {
                    handle_client(stream, shared_data_client, peer_addr);
                });
            }
            Err(e) => eprintln!("Erreur de connexion : {}", e),
        }
    }
}