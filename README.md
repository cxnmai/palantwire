palantir, for pipewire.

lol seriously tho its granola, but the wrapper is free (BYOK, use codex subscription or add support for yours) and CLI markdown render based.
Requires an installed whisper model. You do that yourself, and point palantwire to it.

cargo install --git https://github.com/cxnmai/palantwire

currently the window selection only works for niri, since wayland itself doesn't expose window selection. encourage PRs to add support for more LLM providers and wayland compositors. or if you can figure out a general solution.

could use way better prompt engineering as well.
