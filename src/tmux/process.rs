use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, HashSet},
    process::Command,
};

#[derive(Debug)]
struct Process {
    parent: u32,
    group: i32,
    foreground_group: i32,
}

pub struct ProcessTable(BTreeMap<u32, Process>);

impl ProcessTable {
    pub fn load() -> Result<Self> {
        let output = Command::new("ps")
            .args(["-axo", "pid=,ppid=,pgid=,tpgid="])
            .output()
            .context("read process table")?;
        if !output.status.success() {
            bail!("ps: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Self::parse(&String::from_utf8(output.stdout)?)
    }

    fn parse(text: &str) -> Result<Self> {
        let mut processes = BTreeMap::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() != 4 {
                bail!("invalid ps row: {line}");
            }
            processes.insert(
                fields[0].parse()?,
                Process {
                    parent: fields[1].parse()?,
                    group: fields[2].parse()?,
                    foreground_group: fields[3].parse()?,
                },
            );
        }
        Ok(Self(processes))
    }

    fn foreground_pid(&self, root: u32) -> Option<u32> {
        self.0
            .iter()
            .filter_map(|(&pid, process)| {
                if process.foreground_group <= 0 || process.group != process.foreground_group {
                    return None;
                }
                let mut ancestor = pid;
                let mut seen = HashSet::new();
                while seen.insert(ancestor) {
                    if ancestor == root {
                        return Some((
                            i32::try_from(pid).ok() == Some(process.group),
                            std::cmp::Reverse(seen.len()),
                            pid,
                        ));
                    }
                    ancestor = self.0.get(&ancestor)?.parent;
                }
                None
            })
            .max()
            .map(|(_, _, pid)| pid)
    }

    pub fn foreground_argv(&self, root: u32) -> Result<Vec<String>> {
        match self.foreground_pid(root) {
            Some(pid) => match argv(pid) {
                Ok(argv) => Ok(argv),
                // Process may exit between ps and argv capture.
                Err(_) if !process_exists(pid) => Ok(vec![]),
                Err(error) => Err(error.context(format!("read arguments for process {pid}"))),
            },
            None => Ok(vec![]),
        }
    }

    pub fn foreground_is_current_call(&self, root: u32) -> bool {
        self.foreground_contains(root, std::process::id())
    }

    fn foreground_contains(&self, root: u32, caller: u32) -> bool {
        self.foreground_pid(root).is_some_and(|foreground| {
            self.is_ancestor(foreground, caller) || self.is_ancestor(caller, foreground)
        })
    }

    fn is_ancestor(&self, ancestor: u32, mut pid: u32) -> bool {
        let mut seen = HashSet::new();
        while seen.insert(pid) {
            if pid == ancestor {
                return true;
            }
            let Some(process) = self.0.get(&pid) else {
                break;
            };
            pid = process.parent;
        }
        false
    }
}

fn process_exists(pid: u32) -> bool {
    // SAFETY: signal 0 checks existence without delivering a signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(target_os = "linux")]
fn argv(pid: u32) -> Result<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    bytes
        .strip_suffix(&[0])
        .unwrap_or(&bytes)
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8(arg.to_vec()).context("non-UTF-8 process argument"))
        .collect()
}

#[cfg(target_os = "macos")]
fn argv(pid: u32) -> Result<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, i32::try_from(pid)?];
    let mut size: libc::size_t = 0;
    // SAFETY: MIB is valid; null output requests the required buffer size.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut bytes = vec![0_u8; size];
    // SAFETY: buffer has size bytes and stays live for the duration of sysctl.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    bytes.truncate(size);
    parse_macos_args(&bytes)
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_args(bytes: &[u8]) -> Result<Vec<String>> {
    let count = i32::from_ne_bytes(bytes.get(..4).context("missing argc")?.try_into()?);
    if !(0..=100_000).contains(&count) {
        bail!("invalid argc");
    }
    let executable_end = bytes
        .get(4..)
        .context("missing executable")?
        .iter()
        .position(|byte| *byte == 0)
        .context("unterminated executable")?
        + 4;
    let mut position = executable_end;
    while bytes.get(position) == Some(&0) {
        position += 1;
    }
    let mut args = Vec::new();
    for _ in 0..count {
        let remaining = bytes.get(position..).context("truncated argv")?;
        let length = remaining
            .iter()
            .position(|byte| *byte == 0)
            .context("unterminated argument")?;
        args.push(
            String::from_utf8(remaining[..length].to_vec())
                .context("non-UTF-8 process argument")?,
        );
        position += length + 1;
    }
    Ok(args)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn argv(_: u32) -> Result<Vec<String>> {
    bail!("process capture supports macOS and Linux")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_padded_ps_and_selects_foreground_leader() {
        let table = ProcessTable::parse(
            " 100    1  100  200\n 200 100 200 200\n 201 200 200 200\n 300 100 300 200\n",
        )
        .unwrap();
        assert_eq!(table.foreground_pid(100), Some(200));
        assert_eq!(table.foreground_pid(200), Some(200));
    }
    #[test]
    fn macos_arguments_keep_spaces_empty_values_and_shell_symbols() {
        let mut bytes = 4_i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/tool\0\0tool\0a b\0\0$(no);'\0SECRET=excluded\0");
        assert_eq!(
            parse_macos_args(&bytes).unwrap(),
            ["tool", "a b", "", "$(no);'"]
        );
    }

    #[test]
    fn detects_current_call_and_wrappers_without_discarding_unrelated_jobs() {
        let table = ProcessTable::parse(
            "100 1 100 200\n200 100 200 200\n201 200 200 200\n202 201 200 200\n300 100 300 200\n400 1 400 400\n",
        ).unwrap();
        assert!(table.foreground_contains(100, 200)); // direct invocation
        assert!(table.foreground_contains(100, 201)); // wrapper is group leader
        assert!(table.foreground_contains(100, 202)); // nested wrapper
        assert!(!table.foreground_contains(100, 300)); // background caller
        assert!(!table.foreground_contains(400, 201)); // other pane
    }
}
