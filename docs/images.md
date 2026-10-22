# Images in smash

> Paste, drop or `read()` an image. The model sees it, you see it, and the
> prompt cache keeps it cheap. Same behaviour in the terminal and the window.

Status: **planned** · about 13 h · 5 phases, each shippable on its own.

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
Bytes are read when a request is built and sent as genai `ContentPart::Binary`.

Images a program reads cannot sit inside a tool result on most providers, so
they arrive as one user message appended right after the results:

```text
assistant  ToolCall js { title: "Compare the two screenshots", … }
tool       "[image 1: before.png 1280×720] [image 2: after.png 1280×720]"
user       Image before.png · Image after.png          ← appended, never edited in
```

---

## 4 · Rules

| | Rule | Why |
|---|---|---|
| 💾 | Shrunk once, stored by content hash, identical bytes on every request | prompt cache hits; no re-encoding drift |
| 💰 | Long side ≤ 1568 px and ≤ 1.15 MP; cost estimated as w·h/750 tokens | about 1.6k tokens max (Anthropic's guidance); compaction counts it |
| 👁 | Every image also has a text line: `▣ name W×H` | readable with no graphics at all |
| ✨ | Thumbnails fade in | the motion rule in AGENTS.md |
| 🛟 | Decoding happens off the UI thread; a broken file becomes a ⚠ line | never freezes, never crashes |

---

## 5 · Phases

| # | What ships | Time | Proof |
|---|---|---|---|
| **1** | `Content::Image`, image store, shrinking, sending, token estimate | 3 h | unit tests + DeepSeek describes a real screenshot |
| **2** | Ctrl+V paste, drop a file, `@path`, chips above the prompt | 3 h | key tests; paste a screenshot in both clients |
| **3** | `read()` on image files; images after tool results | 2 h | fake-provider test: message order stays cache-safe |
| **4** | Window thumbnails + click to enlarge; kitty images; half-block fallback | 3–4 h | screenshots of both clients on the ultrawide |
| **5** | Description by a vision model for chat models that can't see; Ctrl+I in the model picker; compaction placeholders | 2 h | fake-provider test; live run with a text-only model |

---

## 6 · Decisions (defaults; change any of them)

- **Who sees the image:** the chat model when its catalog entry says it reads
  images (DeepSeek Flash and Claude Haiku both do).
- **Describer for the rest:** models marked with **Ctrl+I** in the model
  picker, else the cheapest model you have a key for that reads images.
- **Formats:** PNG, JPEG, WebP, GIF (first frame). A shrunk PNG stays PNG, so
  screenshot text stays sharp; anything else becomes JPEG at quality 85.
- **Storage:** `data/images`, shared by every session.
- **New dependency:** the `image` crate (decode, resize, encode). base64 is
  already in.

## 7 · Not in this plan

Image output (none of your models produce images) · PDFs · a `screenshot()`
tool · cleaning up unused images. Each one is added when something needs it.
