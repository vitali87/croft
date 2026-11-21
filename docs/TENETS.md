# Tenets

The non-negotiables behind every decision in croft. The [README](../README.md#tenets) lists them in short; this is what each one means in practice.

1. **croft starts in milliseconds. No exceptions.** From any terminal, any launcher, on any workspace, the first frame is up in milliseconds. Nothing the first frame does not need runs before it: shell probes, image baking, indexing, and network checks all happen after it, off the UI thread.
2. **Speed is a must.** Every feature is weighed against its cost on the hot path before it lands.
3. **Low latency is non-negotiable.** Keystrokes and clicks register instantly; rendering is coalesced so a noisy shell can never starve input.
4. **Local and remote parity always binds.** Behaviour on your Mac and on a Linux box over SSH is identical. There is no second-class remote mode.
5. **The gap between terminal and GUI stays minimal.** croft should look and feel like VS Code, down to the icons and motion.
6. **Everything has a shortcut.** Every action is reachable from the keyboard, and no menu item ships without an accelerator.
7. **Correctness beats workarounds.** Bugs are fixed at the root, never papered over with a fallback or a downgrade.
8. **One binary for the core; extensions arrive in one click.** The binary carries only what every session needs. Everything else is an extension, fetched and verified when you add it from the Extensions view, so there is still nothing to wire up by hand. See [EXTENSIONS.md](EXTENSIONS.md).
