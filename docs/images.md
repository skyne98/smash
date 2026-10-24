# Images in smash

> Paste, drop or `read()` an image. The model sees it, you see it, and the
> prompt cache keeps it cheap. Same behaviour in the terminal and the window.

Status: phases **1–4 done** (sending, attaching, `read()`/`image()`, previews); 5 planned.
Checked against Codex, pi, opencode, Claude Code and Gemini CLI (section 0).

---

## 0 · Prior art (checked on GitHub, 2026-10)

| | Codex (Rust) | pi | opencode | Claude Code | Gemini CLI |
|---|---|---|---|---|---|
| Max size sent | 2048 px, ≤ 2 500 patches of 32 px; `original` up to 6000 px on models that allow it | 2000 × 2000 (`images.autoResize`) | 2000 × 2000 and ≤ 5 MB base64 | 2000 px (a changelog fix cut it from 2576) | none: raw bytes |
| Re-encoding | keeps source bytes when they fit; resized: same format (else PNG), Triangle filter; keeps ICC and EXIF orientation | resizes in a worker; EXIF orientation | Lanczos3; PNG first, then JPEG at falling quality, shrinking by 0.75 until under the byte cap | downscales on paste; converts CMYK | no |
| Repeat cost | LRU cache keyed by SHA-1 | "cache-safe resizing" per model | no | fixed per-turn cache misses in screenshot-heavy sessions | no |
| In context | `<image name=[Image #1] path="…">` … `</image>` around each image | image blocks; pasted image → its file path | data-URI attachment + "Image read successfully" | `[Image #N]` chips; pasted images saved to files Claude can open | inlineData |
| Images from tools | inside the tool result (Responses API `input_image`) | inside the tool result; after the tool results when a provider refuses (Anthropic error results) | inside the tool result | inside the tool result; MCP images also saved to files | inside the tool result |
| Code mode | returns an image object | `read()` returns an image block, `image()` attaches it and saves it to a file | – | – | – |
| Bad images | text placeholder ("cannot attach image … unsupported") | placeholder; warns when the model can't see | 20 MB ingest cap, typed error | placeholder; oversized history images removed and the request retried; 128 MB source cap | 20 MB cap |
| Compaction | image + its tags kept or dropped as one unit, with a token estimate | – | – | strips images first when a request is too large | – |
| Terminal | placeholders | Kitty and iTerm2 protocols (Kitty, Ghostty, WezTerm), 60 cells wide, text fallback | – | `[Image #N]` | – |
| Model can't see | placeholder | warning | – | – | – |

**What this changed in the plan**

1. 2000 px long side and ≤ 5 MB, as a per-model limit (was 1568 px).
2. Source bytes sent untouched when they already fit; resizing keeps format and orientation, then steps down (Codex, opencode).
3. Every image is labelled `[Image #N]` with its path, and its original size when it was shrunk (Codex, Claude Code).
4. `read()` returns an image *block*; only images a program returns or passes to `image()` reach the model (pi). Reading 40 files to check their sizes costs nothing.
5. Broken, oversized or refused images become text placeholders, never a failed turn or a retry loop (Claude Code's changelog is long on this).
6. Kitty **and** iTerm2 protocols (pi); the half-block preview is our own fallback.
7. The describer for models that can't see goes beyond what any of them do; it stays, opt-in.

---

## 1 · What it looks like

### Attaching: same in both clients

```text
  ● The form posts to /login and …
 ─────────────────────────────────────────────────────────────────────
  ▣ login-bug.png  1280×720      ▣ mockup.jpg  900×600
 > why does the button overflow here?
 enter send · backspace removes image · ctrl+p commands     deepseek-flash
```

Images wait above the prompt as chips. **Ctrl+V** pastes, dropping a file
attaches it, `@path/to/shot.png` in the text attaches it too.

### In the transcript: window

```text
 > why does the button overflow here?
   ┌────────────────────────────┐
   │                            │   ▣ login-bug.png
   │      real thumbnail        │     1280×720 · ~1.2k tokens
   │      (≤ 12 rows tall)      │     click to enlarge
   │                            │
   └────────────────────────────┘
 ● The button has a fixed width of 120 px while its label …
```

### In the transcript: terminal

kitty draws the same thumbnail through the kitty graphics protocol. Any
other terminal gets a half-block preview and the same text line:

```text
   ▀▀▄▄▀▀▀▄▄▀▀▄   ▣ login-bug.png
   ▄▀▀▀▄▄▀▀▄▄▀▀     1280×720 · ~1.2k tokens
```

### When the model looks for itself

```text
 ⏵ Compare the two screenshots                         2 calls · 0.1s
   ✓ read   before.png                                    ▣ 1280×720
   ✓ read   after.png                                     ▣ 1280×720
```

### When the chat model can't see

```text
 ⏵ ▣ login-bug.png described by gemini-flash             0.9s · $0.0003
```

---

## 2 · How an image flows

```text
  paste ──┐
  drop  ──┤      decode · shrink · hash         ┌──────────────────────────┐
  @path ──┼──────────────────────────────────▶ │ data/images/<hash>.png   │
  read()──┘         (off the UI thread)         │ written once, never again│
                                                └────────────┬─────────────┘
                                                             │
                              Content::Image { path, mime, width, height }
                                                             │
                                                             ▼
                                         conversation  (append-only)
                                ┌────────────────────────────┴───────────────┐
                                ▼                                            ▼
                    request:  bytes ─▶ base64                  frame:  image slots
                    identical every call                       (row, col, rows, cols)
                    = prompt-cache hit                         gpui img · kitty · ▀▄
```

---

## 3 · Data model

```rust
// smash-ai
pub enum Content {
    Text { text: String },
    Thinking { text: String },
    ToolCall { id: String, name: String, arguments: Value },
    Image { path: PathBuf, mime: String, width: u32, height: u32 },   // new
}
```

Messages carry the path, never the bytes; conversations stay small JSON.
Bytes are read when a request is built and sent as genai `ContentPart::Binary`,
wrapped in a label the model can refer to:

```text
[Image #2] screenshots/login.png · 2000×837 (original 3440×1440)
<the image>
```

The original size lets the model turn what it sees into real pixel positions.

Providers can take images inside a tool result, but genai 0.7 carries tool
results as text only. So, as pi does when Anthropic refuses one, images from a
program arrive as one user message appended right after the results. Append
only, so cache-safe:

```text
assistant  ToolCall js { title: "Compare the two screenshots", … }
tool       "… [Image #1] before.png 1280×720 · [Image #2] after.png 1280×720"
user       Image before.png · Image after.png          ← appended, never edited in
```

---

## 4 · Rules

| | Rule | Why |
|---|---|---|
| 💾 | Prepared once, stored by content hash, identical bytes on every request | prompt cache hits (Codex keeps an LRU for the same reason) |
| 💰 | Long side ≤ 2000 px and ≤ 5 MB (per-model limit); cost estimated as w·h/750 tokens; source files over 50 MB refused | the shared limit across Codex, pi, opencode and Claude Code; compaction counts it |
| 🎯 | Untouched when it already fits; else same format, Lanczos3, EXIF orientation applied, PNG then JPEG 90→60 until under the cap | no needless re-encoding; sharp text in screenshots |
| 🏷 | `[Image #N] path · size (original size)` beside every image | the model can name, re-open and copy it, and map coordinates |
| 🧯 | Unreadable, unsupported or refused → a text placeholder; an image the API rejects is swapped for its placeholder and the call resent once | a bad image never costs a turn or loops |
| 🗜 | Compaction keeps or drops an image with its label as one unit; over the per-request image cap the oldest images become placeholders, in one step | one cache break instead of one per turn |
| 👁 | Every image also has a text line: `▣ name W×H` | readable with no graphics at all |
| ✨ | Thumbnails fade in | the motion rule in AGENTS.md |
| 🛟 | Decoding happens off the UI thread; a broken file becomes a ⚠ line | never freezes, never crashes |

---

## 5 · Phases

| # | What ships | Time | Proof |
|---|---|---|---|
| **1** | `Content::Image`, image store, preparation, labels, placeholders, sending, token estimate | 3–4 h | unit tests (incl. a CMYK JPEG and a zero-byte file) + DeepSeek describes a real screenshot |
| **2** | Ctrl+V paste, drop a file, `@path`, chips above the prompt | 3 h | key tests; paste a screenshot in both clients |
| **3** | `read()` returns an image block (size, path); `image(x)` or returning it attaches it; images after tool results | 2 h | fake-provider test: only attached images are sent, order stays cache-safe |
| **4** | Window thumbnails + click to enlarge; Kitty and iTerm2 images (Kitty, Ghostty, WezTerm); half-block fallback | 3–4 h | screenshots of both clients on the ultrawide |
| **5** | Models that can't see: placeholder + warning by default (as Codex and pi do); opt-in description by a vision model, Ctrl+I in the model picker | 2 h | fake-provider test; live run with a text-only model |

---

## 6 · Decisions (defaults; change any of them)

- **Who sees the image:** the chat model when its catalog entry says it reads
  images (DeepSeek Flash and Claude Haiku both do).
- **Models that can't see:** a placeholder and a warning. Description by a
  vision model only for models you mark with **Ctrl+I** in the model picker.
- **Formats:** PNG, JPEG, WebP, GIF (first frame). HEIC, SVG, TIFF and BMP get a
  placeholder that names the file, which the model can still convert with `sh`.
- **Storage:** `data/images`, shared by every session.
- **New dependency:** the `image` crate (decode, resize, encode). base64 is
  already in.

## 7 · Not in this plan

Image output (none of your models produce images) · PDFs · a `screenshot()`
tool · cleaning up unused images. Each one is added when something needs it.
