# // DEMODEX

## What it is, and isn't

Demodex is a thin layer above (and optionally below) the OpenAI(tm) `codex` cli.

**It is _not_ another harness (like OpenCode), and it does _not_ support different backends (like ACP)**.

Above codex, it provides a webui to remotely access codex sessions, controlling codex cli through the `app-server` JSON-RPC interface.

Below codex, it uses the codex executor interface to run one or more execution backends (where the tools are executed), which can be locally on the host, in a container/VM, or, using a custom SSH executor, access a remote machine through SSH _**without having to install a server on that machine**_.

## Setup

Demodex does not embed codex, you need to install it, depending on your os.

You can configure whether it should share authentication and session storage with your normal user profile, or have it's own codex directory.

The demox webui supports token authentication or tailscale authentication. It does **not** have any multi-user features. There's one token, and one list of sessions.

If you use nix, this repo contains the files I use to host it on my system.

The webui is a PWA and can be pinned to the home screen on mobile devices. It will detect updates when you update the service and prompt for update and reload.

### Recommended setup

Install and configure both codex and tailscale. Ask your agent to scan the repository for malicious code (important!) and to set it up on your system. :)

## Development and Contribution

It works for me, my next planned steps are probably improving the multi-executor workflow and additional tools to copy files and folders between executors.

If you're here, you're a vibecoder. Please don't send PRs, I'm not gonna merge any PRs. Open an issue, if you have changes, push them to a branch and link the branch. I'll tell my agent to look at it.

## License

This is vibe-engineered, as such I'm happy to put it into public domain. My contributions are dual-licensed under cc0 and MIT, at your convenience; for referenced crates and libraries, their respective licenses apply.

# Usage

So, with all that out of the way, how do you actually use it?