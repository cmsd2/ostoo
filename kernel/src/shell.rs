extern crate alloc;

use alloc::string::String;
use alloc::string::ToString;
use libkernel::task::registry;
use libkernel::{print, println};

// ---------------------------------------------------------------------------
// Messages

pub enum ShellMsg {
    /// A complete line of input from the keyboard actor.
    KeyLine(String),
    /// Redraw the prompt (e.g. after returning from a userspace process).
    Reprompt,
}

// ---------------------------------------------------------------------------
// Shell actor

pub struct Shell {
    cwd: spin::Mutex<String>,
}

impl Shell {
    pub fn new() -> Self {
        Shell { cwd: spin::Mutex::new("/".to_string()) }
    }
}

// ---------------------------------------------------------------------------
// Path helpers

fn resolve_path(cwd: &str, path: &str) -> String {
    libkernel::path::resolve(cwd, path)
}

#[devices::actor("shell", ShellMsg)]
impl Shell {
    // ── Startup ──────────────────────────────────────────────────────────
    #[on_start]
    async fn on_start(&self) {
        println!();
        self.print_prompt();
    }

    // ── Message handlers ──────────────────────────────────────────────────
    #[on_message(KeyLine)]
    async fn on_key_line(&self, line: String) {
        self.execute_command(&line).await;
        self.print_prompt();
    }

    #[on_message(Reprompt)]
    async fn on_reprompt(&self) {
        self.print_prompt();
    }

    fn print_prompt(&self) {
        use libkernel::task::mailbox::ActorMsg;
        use crate::keyboard_actor::{KeyboardMsg, KeyboardInfo};

        let cwd = self.cwd.lock().clone();
        let mut prompt = String::from("kernel:");
        prompt.push_str(&cwd);
        prompt.push_str("# ");

        // Notify the keyboard actor so it uses the correct column for cursor
        // positioning and can reprint the prompt on Ctrl+C / Ctrl+L.
        if let Some(kb) = libkernel::task::registry::get::<KeyboardMsg, KeyboardInfo>("keyboard") {
            kb.send(ActorMsg::Inner(KeyboardMsg::SetPrompt(prompt.clone())));
        }
        print!("{}", prompt);
    }

    // ── Command dispatch ──────────────────────────────────────────────────
    async fn execute_command(&self, line: &str) {
        let (cmd, rest) = match line.find(' ') {
            Some(i) => (&line[..i], line[i + 1..].trim()),
            None    => (line, ""),
        };
        match cmd {
            "help"    => cmd_help(),
            "clear"   => libkernel::vga_buffer::clear_content(),
            "echo"    => println!("{}", rest),
            "driver"  => self.cmd_driver(rest).await,
            "blk"     => self.cmd_blk(rest).await,
            "ls"      => self.cmd_blk_ls(rest).await,
            "cat"     => self.cmd_blk_cat(rest).await,
            "pwd"     => self.cmd_pwd(),
            "cd"      => self.cmd_cd(rest).await,
            "mount"   => self.cmd_mount(rest).await,
            "test"    => self.cmd_test(rest).await,
            "exec"    => self.cmd_exec(rest).await,
            "md5"     => self.cmd_md5(rest).await,
            other     => println!("unknown command: '{}'  (try 'help')", other),
        }
    }

    // ── blk command ───────────────────────────────────────────────────────────
    async fn cmd_blk(&self, rest: &str) {
        use libkernel::task::mailbox::ActorMsg;
        use devices::virtio::blk::{VirtioBlkMsg, VirtioBlkInfo};

        let (sub, arg) = match rest.find(' ') {
            Some(i) => (rest[..i].trim(), rest[i + 1..].trim()),
            None    => (rest.trim(), ""),
        };

        match sub {
            "ls"  => self.cmd_blk_ls(arg).await,
            "cat" => self.cmd_blk_cat(arg).await,
            "info" => {
                match libkernel::task::registry::ask_info("virtio-blk").await {
                    Some(s) => {
                        println!("  name:    {}", s.name);
                        println!("  running: {}", s.running);
                        println!("  info:    {:?}", s.info);
                    }
                    None => println!("virtio-blk: not found or not responding"),
                }
            }
            "read" => {
                let sector: u64 = match arg.parse() {
                    Ok(n)  => n,
                    Err(_) => { println!("usage: blk read <sector>"); return; }
                };
                let inbox = match libkernel::task::registry::get::<VirtioBlkMsg, VirtioBlkInfo>(
                    "virtio-blk"
                ) {
                    Some(mb) => mb,
                    None => { println!("virtio-blk: driver not found"); return; }
                };
                let result: Option<Result<alloc::vec::Vec<u8>, ()>> = inbox.ask(|reply| {
                    ActorMsg::Inner(VirtioBlkMsg::Read(sector, reply))
                }).await;
                match result {
                    Some(Ok(buf)) => {
                        println!("sector {}  (first 64 bytes):", sector);
                        let end = 64.min(buf.len());
                        for chunk in buf[..end].chunks(16) {
                            for b in chunk { print!("{:02x} ", b); }
                            println!();
                        }
                    }
                    Some(Err(())) => println!("virtio-blk: read error"),
                    None          => println!("virtio-blk: no response"),
                }
            }
            _ => println!("usage: blk <ls [path]|cat <path>|info|read <sector>>"),
        }
    }

    // ── pwd ───────────────────────────────────────────────────────────────────
    fn cmd_pwd(&self) {
        println!("{}", self.cwd.lock().clone());
    }

    // ── cd ────────────────────────────────────────────────────────────────────
    async fn cmd_cd(&self, path: &str) {
        let cwd    = self.cwd.lock().clone();
        let target = resolve_path(&cwd, if path.is_empty() { "/" } else { path });

        match devices::vfs::list_dir(&target).await {
            Ok(_)                                         => *self.cwd.lock() = target,
            Err(devices::vfs::VfsError::NotFound)         => println!("cd: not found: {}", target),
            Err(devices::vfs::VfsError::NotADirectory)    => println!("cd: not a directory: {}", target),
            Err(e)                                        => println!("cd: {:?}", e),
        }
    }

    // ── blk ls ────────────────────────────────────────────────────────────────
    async fn cmd_blk_ls(&self, path: &str) {
        let cwd  = self.cwd.lock().clone();
        let path = resolve_path(&cwd, path);

        match devices::vfs::list_dir(&path).await {
            Ok(entries) => {
                if entries.is_empty() {
                    println!("  (empty)");
                } else {
                    for e in &entries {
                        if e.is_dir {
                            println!("  [DIR]        {}", e.name);
                        } else {
                            println!("  [FILE {:5}]  {}", e.size, e.name);
                        }
                    }
                }
            }
            Err(e) => println!("error: {:?}", e),
        }
    }

    // ── blk cat ───────────────────────────────────────────────────────────────
    async fn cmd_blk_cat(&self, path: &str) {
        if path.is_empty() {
            println!("usage: cat <path>");
            return;
        }

        let cwd  = self.cwd.lock().clone();
        let path = resolve_path(&cwd, path);

        match devices::vfs::read_file(&path, libkernel::process::ProcessId::KERNEL).await {
            Ok(data) => {
                for &b in &data {
                    if (0x20..0x7F).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
                        print!("{}", b as char);
                    } else {
                        print!(".");
                    }
                }
                println!();
            }
            Err(e) => println!("error: {:?}", e),
        }
    }

    // ── mount ─────────────────────────────────────────────────────────────────
    async fn cmd_mount(&self, rest: &str) {
        use devices::virtio::blk::{VirtioBlkMsg, VirtioBlkInfo};

        let rest = rest.trim();
        if rest.is_empty() {
            // List current mounts.
            devices::vfs::with_mounts(|mounts| {
                if mounts.is_empty() {
                    println!("  (no mounts)");
                } else {
                    for (mp, fs) in mounts {
                        println!("  {}  {}", mp, fs.fs_type());
                    }
                }
            });
            return;
        }

        let (fstype, mountpoint) = match rest.find(' ') {
            Some(i) => (rest[..i].trim(), rest[i + 1..].trim()),
            None    => { println!("usage: mount [<fstype> <mountpoint>]"); return; }
        };

        if mountpoint.is_empty() {
            println!("usage: mount [<fstype> <mountpoint>]");
            return;
        }

        match fstype {
            "proc" => {
                devices::vfs::mount(mountpoint, devices::vfs::AnyVfs::Proc(devices::vfs::ProcVfs));
                println!("mounted proc at {}", mountpoint);
            }
            "blk" => {
                let inbox = match libkernel::task::registry::get::<VirtioBlkMsg, VirtioBlkInfo>(
                    "virtio-blk"
                ) {
                    Some(mb) => mb,
                    None => { println!("virtio-blk: driver not found"); return; }
                };
                devices::vfs::mount(mountpoint, devices::vfs::AnyVfs::Exfat(
                    devices::vfs::ExfatVfs::new(inbox)
                ));
                println!("mounted blk at {}", mountpoint);
            }
            other => println!("unknown filesystem type '{}' (use: proc | blk)", other),
        }
    }

    // ── test ──────────────────────────────────────────────────────────────────
    async fn cmd_test(&self, rest: &str) {
        match rest.trim() {
            "ring3" => {
                let pid = crate::ring3::run_hello_isolated();
                println!("[test ring3] spawned pid {}", pid.as_u64());
            }
            "pagefault" => {
                let pid = crate::ring3::run_pagefault_isolated();
                println!("[test pagefault] spawned pid {}", pid.as_u64());
            }
            "isolation" => {
                let ok = crate::ring3::test_isolation();
                if ok {
                    println!("isolation: PASS — two PML4s have independent mappings");
                } else {
                    println!("isolation: FAIL");
                }
            }
            _ => {
                println!("usage: test <ring3|pagefault|isolation>");
                println!("  ring3      ring-3 write+exit via syscall, machine halts");
                println!("  pagefault  ring-3 touches unmapped address, machine halts");
                println!("  isolation  verify two address spaces are independent (returns)");
            }
        }
    }

    // ── exec ──────────────────────────────────────────────────────────────
    async fn cmd_exec(&self, path: &str) {
        if path.is_empty() {
            println!("usage: exec <path>");
            return;
        }
        let cwd  = self.cwd.lock().clone();
        let path = resolve_path(&cwd, path);

        let data = match devices::vfs::read_file(&path, libkernel::process::ProcessId::KERNEL).await {
            Ok(d) => d,
            Err(e) => { println!("exec: {:?}", e); return; }
        };

        let pid = match crate::ring3::spawn_process(&data) {
            Ok(pid) => pid,
            Err(e) => { println!("exec: {}", e); return; }
        };

        println!("[exec] spawned pid {}", pid.as_u64());
        libkernel::console::set_foreground(pid);
        crate::wait_and_reap(pid).await;
    }

    // ── md5 ──────────────────────────────────────────────────────────────
    async fn cmd_md5(&self, path: &str) {
        if path.is_empty() {
            println!("usage: md5 <path>");
            return;
        }
        let cwd  = self.cwd.lock().clone();
        let path = resolve_path(&cwd, path);

        match devices::vfs::read_file(&path, libkernel::process::ProcessId::KERNEL).await {
            Ok(data) => {
                let digest = libkernel::md5::compute(&data);
                println!("{}  {}", libkernel::md5::hex(&digest), path);
            }
            Err(e) => println!("md5: {:?}", e),
        }
    }

    async fn cmd_driver(&self, rest: &str) {
        let (subcmd, name) = match rest.find(' ') {
            Some(i) => (rest[..i].trim(), rest[i + 1..].trim()),
            None    => (rest.trim(), ""),
        };
        match subcmd {
            "start" => {
                if name.is_empty() {
                    println!("usage: driver start <name>");
                } else {
                    match devices::driver::start_driver(name) {
                        Ok(())   => println!("driver '{}' started", name),
                        Err(msg) => println!("error: {}", msg),
                    }
                }
            }
            "stop" => {
                if name.is_empty() {
                    println!("usage: driver stop <name>");
                } else {
                    match devices::driver::stop_driver(name) {
                        Ok(())   => println!("driver '{}' stop requested", name),
                        Err(msg) => println!("error: {}", msg),
                    }
                }
            }
            "info" => {
                if name.is_empty() {
                    println!("usage: driver info <name>");
                } else if name == "shell" {
                    // Sending ErasedInfo to our own mailbox would deadlock —
                    // we can't recv() while blocked executing this command.
                    println!("  name:    shell");
                    println!("  running: true");
                } else {
                    match registry::ask_info(name).await {
                        Some(s) => {
                            println!("  name:    {}", s.name);
                            println!("  running: {}", s.running);
                            println!("  info:    {:?}", s.info);
                        }
                        None => println!("error: '{}' not found or not responding", name),
                    }
                }
            }
            _ => println!("usage: driver <start|stop|info> <name>"),
        }
    }
}

// ---------------------------------------------------------------------------
// help

fn cmd_help() {
    println!("Commands:");
    println!("  help              show this message");
    println!("  clear             clear the screen");
    println!("  echo <text>       print text back");
    println!("  driver start <n>  start a driver by name");
    println!("  driver stop <n>   stop a driver by name");
    println!("  driver info <n>   query driver info");
    println!("  blk info          virtio-blk device info");
    println!("  blk read <n>      hex-dump sector N from virtio-blk");
    println!("  blk ls [path]     list exFAT directory (default: /)");
    println!("  blk cat <path>    print exFAT file as text");
    println!("  ls [path]         list directory via VFS");
    println!("  cat <path>        print file via VFS");
    println!("  pwd               print working directory");
    println!("  cd [path]         change working directory");
    println!("  mount             list mounted filesystems");
    println!("  mount proc <mp>   mount procfs at <mountpoint>");
    println!("  mount blk <mp>    mount exFAT block device at <mountpoint>");
    println!("  md5 <path>        print MD5 hash of a file");
    println!("  exec <path>       load and run an ELF binary from the VFS");
    println!("  test ring3        ring-3 write+exit via syscall (spawns process)");
    println!("  test pagefault    ring-3 page fault on unmapped addr (spawns process)");
    println!("  test isolation    verify two PML4s are independent (returns)");
    println!();
    println!("System info available via: cat /proc/<file>");
    println!("  cpuinfo meminfo memmap pmap threads tasks");
    println!("  idt pci lapic ioapic drivers uptime");
}
