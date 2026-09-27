# // DEMODEX

## What it is, and isn't

Demodex is a thin layer above (and optionally below) the OpenAI(tm) `codex` cli.

**It is _not_ another harness (like OpenCode), and it does _not_ support different backends (like ACP)**.

Above codex, it provides a webui to remotely access codex sessions, controlling codex cli through the `app-server` JSON-RPC interface.

Below codex, it uses the codex executor interface to run zero or more execution targets (where tools run). Targets can be the local host, a managed Docker or Podman container, a managed NixOS VM, an external executor, or a remote machine reached through SSH _**without having to install a server on that machine**_.

## Setup

Demodex does not embed codex, you need to install it, depending on your os.

In native host mode, you can give Demodex its own Codex profile or explicitly reuse your normal user profile.

The Demodex web UI supports a local token or an explicitly allowed Tailscale Serve identity through its private socket. It does **not** have multi-user features: there is one session store per daemon.

If you use nix, this repo contains the files I use to host it on my system.

The web UI is a PWA and can be pinned to the home screen on mobile devices. It detects new static frontend releases and prompts for update and reload.

### Recommended setup

Install and configure both codex and tailscale. Ask your agent to scan the repository for malicious code (important!) and to set it up on your system. :)

## Development and Contribution

It works for me, my next planned steps are probably improving the multi-executor workflow and additional tools to copy files and folders between executors.

If you're here, you're a vibecoder. Please don't send PRs, I'm not gonna merge any PRs. Open an issue, if you have changes, push them to a branch and link the branch. I'll tell my agent to look at it.

## License

This is vibe-engineered, as such I'm happy to put it into public domain. My contributions are dual-licensed under cc0 and MIT, at your convenience; for referenced crates and libraries, their respective licenses apply.

# Usage

Start the daemon, open its web UI and sign in to Codex. Create a session with zero or more execution targets. You can change targets later from Session controls. During active work, choose **Save for next turn** or **Interrupt and save**. The latter waits for the turn to stop; the next explicit new turn uses the saved selection. Neither action starts a hidden turn or terminates background jobs.

Server Settings creates shared VMs and containers. For a container, choose Docker or Podman and an image already present in that engine's local image store, then set memory and CPU limits. The daemon user needs access to the chosen engine. Each container keeps a private workspace and home across stop/start. Its tools require the danger-full-access sandbox setting. Each container gets its own isolated bridge network with outbound access and a loopback-only executor port. Podman requires version 6 or newer and Netavark with strict bridge isolation support. It does not receive your host home or engine socket. On Nix hosts, Demodex mounts `/nix/store` read-only to run the installed Codex executable; elsewhere the image must contain `codex`.

Create a session-specific SSH executor in New Session or Session controls. It uses the remote account's existing SSH access and does not install a helper there. Existing sessions can also select shared targets from their controls. NixOS containers managed by `nixos-container` are distinct from the Docker and Podman containers supported here.
