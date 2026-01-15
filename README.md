# SSH Liaison MCP Server

<div align="center">

**Stateful SSH connection and command execution via Model Context Protocol (MCP)**

[![Rust](https://img.shields.io/badge/rust-1.0+-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Version](https://img.shields.io/badge/version-0.1.0-blue.svg)](https://github.com/Citizen4our/ssh-liaison-mcp)

</div>

---

## ✨ Features

| Feature | Description |
|---------|-------------|
| 🔄 **Stateful Sessions** | **Core feature**: Shell state (current directory, environment variables, working directory) is preserved between MCP tool calls. Each command runs in the same persistent shell session, allowing multi-step workflows. |
| 🔌 **MCP Server Mode** | Integrate with Cursor/Claude Desktop for AI-assisted SSH operations with stateful command execution |
| ⚙️ **SSH Config Support** | Uses `~/.ssh/config` for host aliases and connection parameters |
| 💻 **Standalone CLI Mode** | Interactive terminal for debugging and testing (see below) |

---

## 📦 Installation

### Build from Source

```bash
git clone <repository-url>
cd ssh-liaison-mcp
cargo build --release
```

The binary will be located at `target/release/ssh-liaison-mcp`.

---

## 🚀 Usage

### MCP Server Mode (Primary Use Case)

The main feature of this server is **stateful SSH sessions** - each SSH connection maintains a persistent shell session where state (current directory, environment variables, etc.) is preserved between MCP tool calls. This enables natural multi-step workflows where commands build upon each other.

#### How Stateful Sessions Work

When you connect to a host via `ssh_connect`, a persistent shell session is established. All subsequent `ssh_run_command` calls for that host execute in the **same shell session**, meaning:

- **Current directory is preserved**: If you `cd /var/log` in one command, the next command starts from `/var/log`
- **Environment variables persist**: Variables set with `export` remain available in subsequent commands
- **Shell state is maintained**: History, aliases, and other shell state persist between calls
- **Efficient**: No need to reconnect or re-establish context for each command

**Example workflow:**
```
1. ssh_connect("production") → Establishes persistent shell
2. ssh_run_command("production", "cd /var/log") → Changes directory
3. ssh_run_command("production", "pwd") → Returns "/var/log" (state preserved!)
4. ssh_run_command("production", "ls -la") → Lists files in /var/log
```

#### For Cursor IDE

1. **Build the binary:**
   ```bash
   cargo build --release
   ```

2. **Add to Cursor settings** (`~/.cursor/mcp.json` or Cursor settings UI):
   ```json
   {
     "mcpServers": {
       "ssh-liaison": {
         "command": "/absolute/path/to/ssh-liaison-mcp",
         "args": ["serve"]
       }
     }
   }
   ```

3. **Restart Cursor**

#### For Claude Desktop

1. **Build the binary:**
   ```bash
   cargo build --release
   ```

2. **Add to Claude Desktop config** (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):
   ```json
   {
     "mcpServers": {
       "ssh-liaison": {
         "command": "/absolute/path/to/ssh-liaison-mcp",
         "args": ["serve"]
       }
     }
   }
   ```

3. **Restart Claude Desktop**

---

### Legacy Direct Connect Mode

For backward compatibility:

```bash
cargo run -- connect <user> <host> [--port <port>]
```

---

## 🔧 SSH Configuration

The server reads from `~/.ssh/config` for host aliases. This is the recommended way to connect as it centralizes connection settings.

### Example SSH Config

```ssh-config
# Simple host alias
Host rpi
    HostName 192.168.1.100
    User pi
    Port 22
    IdentityFile ~/.ssh/id_ed25519

# Production server with custom port
Host production
    HostName prod.example.com
    User deploy
    Port 2222
    IdentityFile ~/.ssh/deploy_key

# Development server
Host dev
    HostName dev.example.com
    User developer
    IdentityFile ~/.ssh/id_rsa
```

## 🛠️ MCP Tools

When running as MCP server, the following tools are available:

| Tool | Description | Parameters |
|------|-------------|------------|
| **ssh_connect** | Connect to remote SSH server and establish a **persistent shell session**. The session maintains state between subsequent command calls. | `host_alias` (string) - Host alias defined in SSH config |
| **ssh_run_command** | Execute command on connected host in the **same persistent shell session**. Current directory, environment variables, and shell state are preserved from previous commands. | `host` (string) - Host alias, `command` (string) - Command to execute |
| **ssh_read_log** | Read last N lines from log file using the persistent shell session. | `host` (string) - Host alias, `file_path` (string) - Path to log file, `lines` (integer) - Number of lines to read |

**Important Notes:**
- **Stateful**: All commands for the same host run in the same persistent shell session
- **SSH Config Required**: In MCP mode, connections use SSH config aliases. For direct connections (user/hostname/password), use CLI mode instead
- **Session Persistence**: Once connected, the session remains active until the MCP server is restarted or explicitly disconnected

### Example Prompts for MCP Mode

Once the MCP server is configured, you can ask the AI assistant to perform SSH operations:

**Example 1:**
```
User: Connect to rpi and show me the current directory and disk usage
```

**AI Response:**
```
I'll connect to rpi and check the current directory and disk usage for you.

[Connecting to rpi...]

Current directory: /home/pi
Disk usage:
Filesystem      Size  Used Avail Use% Mounted on
/dev/root        15G  5.2G  8.8G  38% /
/dev/mmcblk0p1  253M   49M  204M  20% /boot
```

**Example 2:**
```
User: Connect to production and read the last 50 lines from /var/log/nginx/error.log
```

**AI Response:**
```
I'll connect to production and read the last 50 lines from the nginx error log.

[Connecting to production...]

Last 50 lines from /var/log/nginx/error.log:
2024-01-15 10:23:45 [error] 12345#0: *123 connect() failed (111: Connection refused)
2024-01-15 10:24:12 [error] 12345#0: *124 upstream timed out
...
```

**Example 3:**
```
User: Check system uptime and memory usage on the dev server
```

**AI Response:**
```
I'll check the system uptime and memory usage on the dev server.

[Connecting to dev...]

System uptime: 15 days, 3 hours, 22 minutes
Memory usage:
              total        used        free      shared  buff/cache   available
Mem:           7.8G        2.1G        3.2G        128M        2.5G        5.4G
Swap:          2.0G          0B        2.0G
```

**Example 4:**
```
User: Show me running processes and check if nginx service is running on production
```

**AI Response:**
```
I'll check the running processes and verify if nginx is running on production.

[Connecting to production...]

Nginx service status: active (running)
Top processes by CPU:
  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 nginx     20   0  123456   4567   1234 S   2.3   0.1   0:05:23 nginx
 5678 app       20   0  234567   8901   2345 S   1.8   0.2   0:12:45 app
...
```

#### Stateful Session Example

This example demonstrates how state is preserved between commands:

```
User: Connect to rpi, navigate to /var/log, and show me the last 10 lines of syslog
```

**AI Response:**
```
I'll connect to rpi, navigate to /var/log, and show you the last 10 lines of syslog.

[Connecting to rpi...]
[Changing directory to /var/log...]

Last 10 lines of syslog:
Jan 15 10:23:45 rpi systemd[1]: Started Network Manager.
Jan 15 10:24:12 rpi sshd[1234]: Accepted publickey for pi from 192.168.1.50
...
```

**What happened behind the scenes:**
1. `ssh_connect("rpi")` - Established persistent shell session
2. `ssh_run_command("rpi", "cd /var/log")` - Changed directory (state saved)
3. `ssh_run_command("rpi", "tail -n 10 syslog")` - Executed from `/var/log` (state preserved!)

The second command automatically started from `/var/log` because the shell state was preserved from the previous command.

---

### Standalone CLI Mode (For Debugging)

Interactive terminal mode for testing and debugging. Useful for troubleshooting connection issues or testing commands manually.

#### Connection Methods

**1. Using SSH Config Alias**

```bash
# Connect immediately using alias from ~/.ssh/config
cargo run -- cli --host rpi
# or
./target/release/ssh-liaison-mcp cli --host rpi
```

**2. Direct Connection via Command Line**

```bash
# Direct connection with SSH keys (default port 22)
cargo run -- cli --user pi --hostname 192.168.1.100

# Direct connection with custom port
cargo run -- cli --user pi --hostname 192.168.1.100 --port 2222

# Direct connection with password authentication
cargo run -- cli --user pi --hostname 192.168.1.100 --password mypassword

# Direct connection with password and custom port
cargo run -- cli --user pi --hostname 192.168.1.100 --password mypassword --port 2222
```

#### Connection Examples

```bash
# Example 1: Connect via SSH config
cargo run -- cli --host production

# Example 2: Direct connection to Raspberry Pi
cargo run -- cli --user pi --hostname 192.168.1.100

# Example 3: Direct connection with password to custom port
cargo run -- cli --user admin --hostname server.example.com --password secret --port 2222

# Example 4: Interactive mode - connect later
cargo run -- cli
ssh> connect dev-server
[dev-server]> uname -a
[dev-server]> exit
```

---


## 🔐 Authentication

The server attempts authentication in the following order:

1. **SSH agent** (if available)
2. **Identity file** from SSH config
3. **Common SSH keys** (in order):
   - `~/.ssh/id_ed25519`
   - `~/.ssh/id_rsa`
   - `~/.ssh/id_ecdsa`
   - `~/.ssh/id_dsa`

---

## ⚠️ Security Notes

- **Read-only operations recommended**: The tools include warnings about destructive operations
- **Password handling**: Sudo password elicitation support is planned but not yet fully implemented
- **No password logging**: Passwords are never logged or exposed

---

## 🧪 Development

```bash
# Run in development mode
cargo run -- cli --host <your-host>

# Build release
cargo build --release

# Run tests
cargo test
```

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
