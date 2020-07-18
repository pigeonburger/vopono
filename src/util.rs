use super::list::get_lock_namespaces;
use anyhow::{anyhow, Context};
use directories_next::BaseDirs;
use ipnet::Ipv4Net;
use log::{debug, info, warn};
use nix::unistd::{Group, User};
use regex::Regex;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use users::{get_current_uid, get_user_by_uid};
use walkdir::WalkDir;

pub fn config_dir() -> anyhow::Result<PathBuf> {
    let mut pathbuf = PathBuf::new();
    let _res: () = if let Some(base_dirs) = BaseDirs::new() {
        pathbuf.push(base_dirs.config_dir());
        Ok(())
    } else if let Ok(user) = std::env::var("SUDO_USER") {
        // TODO: DRY
        let confpath = format!("/home/{}/.config", user);
        let path = Path::new(&confpath);
        if path.exists() {
            pathbuf.push(path);
            Ok(())
        } else {
            Err(anyhow!("Could not find valid config directory!"))
        }
    } else if let Some(user) = get_user_by_uid(get_current_uid()) {
        let confpath = format!("/home/{}/.config", user.name().to_str().unwrap());
        let path = Path::new(&confpath);
        if path.exists() {
            pathbuf.push(path);
            Ok(())
        } else {
            Err(anyhow!("Could not find valid config directory!"))
        }
    } else {
        Err(anyhow!("Could not find valid config directory!"))
    }?;
    Ok(pathbuf)
}

// TODO: DRY with above
pub fn get_username() -> anyhow::Result<String> {
    if let Ok(user) = std::env::var("SUDO_USER") {
        Ok(user)
    } else if let Some(user) = get_user_by_uid(get_current_uid()) {
        Ok(String::from(
            user.name().to_str().expect("Invalid username"),
        ))
    } else {
        Err(anyhow!("No valid username!"))
    }
}

pub fn get_group(username: &str) -> anyhow::Result<String> {
    let user = User::from_name(username)?;

    match user {
        Some(x) => Ok(Group::from_gid(x.gid)?
            .expect("Failed to use group id")
            .name),
        None => Ok(username.to_string()),
    }
}

pub fn set_config_permissions() -> anyhow::Result<()> {
    let mut check_dir = config_dir()?;
    check_dir.push("vopono");
    let username = get_username()?;
    let group = get_group(&username)?;
    sudo_command(&[
        "chown",
        "-R",
        username.as_str(),
        check_dir.to_str().expect("No valid config dir"),
    ])?;
    sudo_command(&[
        "chgrp",
        "-R",
        group.as_str(),
        check_dir.to_str().expect("No valid config dir"),
    ])?;
    Ok(())
}

pub fn get_allocated_ip_addresses() -> anyhow::Result<Vec<Ipv4Net>> {
    let output = Command::new("ip")
        .args(&["addr", "show", "type", "veth"])
        .output()?
        .stdout;
    let output = std::str::from_utf8(&output)?;
    debug!("Existing interfaces: {}", output);

    let re = Regex::new(r"inet\s+(?P<ip>\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}/\d{1,2})").unwrap();
    let mut ips = Vec::new();
    for caps in re.captures_iter(output) {
        ips.push(Ipv4Net::from_str(&caps["ip"])?);
    }
    debug!("Assigned IPs: {:?}", &ips);
    Ok(ips)
}

// pub fn get_veth_ipv4(if_name: &str) -> anyhow::Result<Option<Ipv4Net>> {
//     let output = Command::new("ip")
//         .args(&["addr", "show", "type", "veth", if_name])
//         .output()?
//         .stdout;
//     let output = std::str::from_utf8(&output)?;

//     let re = Regex::new(r"inet\s+(?P<ip>\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}/\d{1,2})").unwrap();
//     let ip = match re.captures_iter(output).next() {
//         None => None,
//         Some(x) => Some(Ipv4Net::from_str(&x["ip"])?),
//     };
//     Ok(ip)
// }

pub fn get_existing_namespaces() -> anyhow::Result<Vec<String>> {
    let output = Command::new("ip").args(&["netns", "list"]).output()?.stdout;
    let output = std::str::from_utf8(&output)?
        .split('\n')
        .map(|x| x.split_whitespace().next())
        .filter(|x| x.is_some())
        .map(|x| String::from(x.unwrap()))
        .collect();
    debug!("Existing namespaces: {:?}", output);

    Ok(output)
}

pub fn check_process_running(pid: u32) -> anyhow::Result<bool> {
    let output = Command::new("ps")
        .args(&["-p", &pid.to_string(), "-o", "pid:1", "--no-headers"])
        .output()?
        .stdout;
    let output = std::str::from_utf8(&output)?.split('\n').next();
    if let Some(x) = output {
        Ok(x.trim() == pid.to_string())
    } else {
        Ok(false)
    }
}

pub fn get_all_running_pids() -> anyhow::Result<Vec<u32>> {
    let output = Command::new("ps")
        .args(&["a", "-o", "pid:1", "--no-headers"])
        .output()?
        .stdout;
    std::str::from_utf8(&output)?
        .split('\n')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(|x| match x.parse::<u32>() {
            Ok(x) => Ok(x),
            Err(_) => Err(anyhow!("Could not parse PID to u32: {:?}", x)),
        })
        .collect()
}

pub fn get_target_subnet() -> anyhow::Result<u8> {
    // TODO: Fix hard limit of <254 vopono instances
    let assigned_ips = get_allocated_ip_addresses()?;
    let mut target_ip = 1;
    while target_ip <= 254 {
        let ip = Ipv4Net::new(Ipv4Addr::new(10, 200, target_ip, 1), 24)?;
        if assigned_ips.contains(&ip) {
            target_ip += 1;
        } else {
            return Ok(target_ip);
        }
    }
    Err(anyhow!(
        "Could not find free subnet of form: 10.200.xxx.1/24"
    ))
}

// TODO: Fix deprecated name
pub fn sudo_command(command: &[&str]) -> anyhow::Result<()> {
    debug!("{}", command.join(" "));

    let (start_command, args) = command
        .split_first()
        .expect("Could not split command slice");

    let exit_status = Command::new(start_command)
        .args(args)
        .status()
        .with_context(|| format!("Failed to run command: {}", command.join(" ")))?;

    if exit_status.success() {
        Ok(())
    } else {
        Err(anyhow!("Command failed: {}", command.join(" ")))
    }
}

// TODO: Clean this up (can we combine maps and filters?)
pub fn clean_dead_locks() -> anyhow::Result<()> {
    let running_processes = get_all_running_pids()?;
    let mut lockfile_path = config_dir()?;
    lockfile_path.push("vopono/locks");

    if lockfile_path.exists() && lockfile_path.read_dir()?.next().is_some() {
        debug!("Cleaning dead lock files...");
        // Delete files if their PIDs are no longer running
        std::fs::create_dir_all(&lockfile_path)?;
        WalkDir::new(&lockfile_path)
            .into_iter()
            .filter(|x| x.is_ok())
            .map(|x| x.unwrap())
            .filter(|x| x.path().is_file())
            .map(|x| {
                (
                    x.clone(),
                    x.file_name()
                        .to_str()
                        .expect("Failed to parse file name")
                        .parse::<u32>()
                        .ok(),
                )
            })
            .filter(|x| x.1.is_some())
            .map(|x| (x.0, running_processes.contains(&x.1.unwrap())))
            .filter(|x| !x.1)
            .map(|x| {
                debug!("Removing lockfile: {}", x.0.path().display());
                std::fs::remove_file(x.0.path())
            })
            .collect::<Result<(), _>>()?;

        // Delete subdirectories if they contain no locks (ignore errors)
        WalkDir::new(&lockfile_path)
            .into_iter()
            .filter(|x| x.is_ok())
            .map(|x| x.unwrap())
            .filter(|x| x.path().is_dir())
            .map(|x| std::fs::remove_dir(x.path()))
            .collect::<Result<(), _>>()
            .ok();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Ok(())
}

pub fn clean_dead_namespaces() -> anyhow::Result<()> {
    let lock_namespaces = get_lock_namespaces()?;
    let existing_namespaces = get_existing_namespaces()?;

    existing_namespaces
        .into_iter()
        .filter(|x| !lock_namespaces.contains_key(x))
        .map(|x| {
            debug!("Removing dead namespace: {}", x);
            sudo_command(&["ip", "netns", "delete", x.as_str()])
        })
        .collect::<Result<(), _>>()?;

    // TODO - deserialize to struct without Drop instead
    let lock_namespaces = Box::new(lock_namespaces);
    Box::leak(lock_namespaces);
    Ok(())
}

pub fn elevate_privileges() -> anyhow::Result<()> {
    // Check if already running as root
    if nix::unistd::getuid().as_raw() != 0 {
        info!("Calling sudo for elevated privileges, current user will be used as default user");
        let args: Vec<String> = std::env::args().collect();

        debug!("Args: {:?}", &args);
        Command::new("sudo").arg("-E").args(args).status()?;
        // Do we want to block here to ensure stdout kept alive? Does it matter?
        std::process::exit(0);
    } else if std::env::var("SUDO_USER").is_err() {
        warn!("Running vopono as root user directly!");
    }
    Ok(())
}
