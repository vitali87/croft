# async-lsp 0.2.4, patched for croft

This is the crates.io release of [async-lsp](https://github.com/oxalica/async-lsp)
0.2.4, MIT OR Apache-2.0, with one change. croft builds it from here through
`[patch.crates-io]` in its own `Cargo.toml`.

Its `Cargo.toml` is the published one without the example, test and bench
targets and the dev-dependencies, whose files are not copied here, and with
serde_json's `raw_value` feature on.

## The change: a message from the peer is parsed once

Published, `Message::read` decoded every incoming message as a
`RawMessage<Message>`: a `#[serde(flatten)]` over an `#[serde(untagged)]` enum.
For that shape serde buffers the whole message into its own private tree,
copies the tree for each variant it tries, and builds a `serde_json::Value` from
the copy, which the waiting request then converts into its own type. A reply was
three trees before anyone read it. For the semantic tokens of a 2 MB source file
from rust-analyzer that was about 90 MB, briefly, inside croft.

Now:

- `IncomingMessage` reads the message field by field, and `into_message` tells
  a request, a response and a notification apart as the untagged enum did: an
  id and a method make a request, an id alone a response, a method alone a
  notification, and a `null` id counts as none.
- A response's `result` stays the JSON text it arrived as (`Box<RawValue>`), and
  the request waiting for it decodes that text straight into its own result
  type. An absent result reads as `null`, as `unwrap_or_default` of an absent
  `Value` did.
- A response croft sends back is serialized into a `RawValue` when it is made.

Requests and notifications from the peer keep their `params` as a `Value`, as
published: that type is public (`AnyRequest`, `AnyNotification`).

`cargo test -p async-lsp --lib` runs this crate's tests, the patch's among
them; croft's CI runs them beside its own.

Drop this copy, and the `[patch.crates-io]` entry, once upstream reads messages
without the buffering.
