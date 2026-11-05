# Private speech worker

Rust service using transcribe-cpp's Cohere implementation. It keeps a locally
provisioned GGUF model resident, receives bounded 16 kHz mono PCM on a Unix socket,
and returns final text. No audio/transcript files or network listeners.

```
nix-shell crates/stt/shell.nix
cargo build -p demodex-stt --release --features vulkan
```

`--features rocm` needs ROCm development tools; the supplied shell supports CPU
(`engine`) and Vulkan. At runtime the chosen backend must exist; no CPU fallback.
The module `../../nix/stt-service.nix` configures systemd. Set enable, user, binary,
model and backend. Its socket is private to that user at
`/run/demodex-stt/stt.sock`. SSH runs the installed `demodex-stt connect` helper.
The binary path can point to a checkout release. Preserve its Nix runtime closure.

Daemon configuration (`--stt-config`):

```json
{
  "destination": "stt@stt-host",
  "socket": "/run/demodex-stt/stt.sock",
  "program": "/run/current-system/sw/bin/demodex-stt"
}
```

Optional absolute `identity_file` and `known_hosts_file` select private SSH
configuration. Provision credentials and trusted host keys before use. The
browser cannot select destinations or commands. Remote helper paths accept only
ASCII letters/digits, `/._-`; destination accepts letters/digits and `@._-`.

Wire version 1: big-endian u32 JSON byte length followed by serde Request/Reply.
Start declares version and language; each Audio carries sequence and PCM16;
Finish returns text. EOF cancels. Maximum frame 128 KiB, chunk 8,000 samples,
recording 180 seconds, idle wait 15 seconds, whole connection 360 seconds.
One recording/inference at a time; other connections receive a busy error.
Native cancellation is cooperative between decode steps, with the slot retained
until the native call returns. The daemon bounds finish to 180 seconds.

The worker currently requires an explicit Cohere language; the PWA offers English
and German. Mixed-language recognition is not automatic. Digital silence returns
empty text; other noise may still be transcribed by the model. No native partial
recognition, VAD or automatic transcript correction is claimed.

Validated model: handy-computer/cohere-transcribe-03-2026-gguf revision
7353b6040bdc1be9e5a3823d99d5fe12afe88264, cohere-transcribe-03-2026-Q8_0.gguf.
SHA256: 931916663432fd895423a4291a8400221802b288967ca2d435fc5e3141c9e71e.
The service never downloads weights; provision the model before enabling it.
