# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Handy is a cross-platform desktop application for offline speech-to-text transcription built with Tauri (Rust backend + React/TypeScript frontend). The app minimizes to the system tray, listens for global keyboard shortcuts, records audio with Voice Activity Detection (VAD), transcribes using local Whisper or Parakeet models, and pastes the result into the active application.

**Core Philosophy:** Handy aims to be the most forkable speech-to-text app - free, open source, privacy-focused (everything runs locally), and built with a simple, extensible architecture.

## Development Commands

### Prerequisites

- [Rust](https://rustup.rs/) (latest stable)
- [Bun](https://bun.sh/) package manager
- Platform-specific build tools (see BUILD.md)

### Essential Commands

```bash
# Install dependencies
bun install

# Download required VAD model (first time setup)
mkdir -p src-tauri/resources/models
curl -o src-tauri/resources/models/silero_vad_v4.onnx https://blob.handy.computer/silero_vad_v4.onnx

# Run in development mode
bun run tauri dev
# If cmake error on macOS:
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev

# Build for production
bun run tauri build

# Format code
bun run format              # Format both frontend and backend
bun run format:check        # Check formatting
bun run format:frontend     # Format frontend only (Prettier)
bun run format:backend      # Format backend only (cargo fmt)

# Frontend-only development
bun run dev                 # Start Vite dev server
bun run build               # Build frontend (TypeScript + Vite)
bun run preview             # Preview built frontend
```

### Running Tests

There are no automated tests in this codebase yet. Testing is currently done manually:
- Run `bun run tauri dev` and test manually with different audio devices
- Use debug mode (`Cmd+Shift+D` on macOS, `Ctrl+Shift+D` on Windows/Linux) to access diagnostic information
- Build production artifacts with `bun run tauri build` and test the installed application

## Architecture

### High-Level Overview

Handy uses a **Manager Pattern** where core functionality is organized into singleton managers that are initialized at startup and managed via Tauri's state system. Communication follows a **Command-Event Architecture**: the frontend calls backend commands via Tauri, and the backend sends updates back via events.

The audio processing pipeline follows this flow:
**Microphone → AudioRecorder → VAD (Voice Activity Detection) → Whisper/Parakeet → Text → Clipboard → Target Application**

### Backend Structure (Rust - `src-tauri/src/`)

**Core Entry Point:**
- `lib.rs` - Application initialization, Tauri setup, manager registration, tray menu, single-instance handling, and logging configuration
- `main.rs` - Minimal entry point that calls into lib.rs

**Managers (`managers/`)** - Core business logic organized as stateful singletons:
- `audio.rs` - AudioRecordingManager handles audio device enumeration, recording lifecycle, microphone muting, and audio feedback
- `model.rs` - ModelManager handles downloading, caching, and loading Whisper and Parakeet models
- `transcription.rs` - TranscriptionManager orchestrates the transcription pipeline (audio → VAD → model → text)
- `history.rs` - HistoryManager stores transcription history in SQLite database

**Audio Toolkit (`audio_toolkit/`)** - Low-level audio processing:
- `audio/` - Device enumeration, recording using cpal, audio resampling with rubato
- `vad/` - Voice Activity Detection using Silero VAD (ONNX model) with SmoothedVad wrapper for stability
- `text.rs` - Text processing utilities for transcriptions

**Commands (`commands/`)** - Tauri command handlers that expose Rust functionality to the frontend:
- `audio.rs` - Audio device listing, recording control
- `history.rs` - Transcription history CRUD operations
- `models.rs` - Model download and management
- `transcription.rs` - Transcription control and settings
- `mod.rs` - Command registration

**Other Key Modules:**
- `shortcut.rs` - Global keyboard shortcut registration and handling using rdev
- `settings.rs` - Settings schema and persistence using tauri-plugin-store
- `clipboard.rs` - Clipboard manipulation and text pasting with platform-specific implementations
- `overlay.rs` - Recording overlay window management
- `tray.rs` - System tray icon and menu
- `actions.rs` - Recording actions and workflow orchestration
- `audio_feedback.rs` - Audio playback for UI feedback sounds
- `signal_handle.rs` - Unix signal handling (SIGUSR2 for recording toggle)

### Frontend Structure (React/TypeScript - `src/`)

**Main Application:**
- `App.tsx` - Root component with onboarding flow, sidebar navigation, and settings pages
- `main.tsx` - React app entry point

**Components (`components/`):**
- `settings/` - Settings UI components (audio, keyboard shortcuts, models, history, etc.)
- `model-selector/` - Model download and selection interface
- `onboarding/` - First-run setup wizard
- `Sidebar.tsx` - Navigation sidebar
- `AccessibilityPermissions.tsx` - Platform permissions UI
- `ui/` - Reusable UI components (buttons, switches, etc.)

**Overlay (`overlay/`):**
- `RecordingOverlay.tsx` - Separate window showing recording status during transcription
- Built as a separate Vite entry point (see vite.config.ts)

**State Management:**
- `stores/settingsStore.ts` - Zustand store for settings state management
- `hooks/` - React hooks for settings and model management

**Type Definitions:**
- `bindings.ts` - Auto-generated TypeScript bindings from Rust types using tauri-specta
- `lib/types.ts` - Shared TypeScript type definitions

### Key Technology Dependencies

**Rust:**
- `whisper-rs` - Local Whisper inference with GPU acceleration (Metal/Vulkan/CUDA)
- `transcribe-rs` - CPU-optimized Parakeet model inference
- `cpal` - Cross-platform audio I/O
- `vad-rs` - Silero VAD for voice activity detection
- `rdev` - Global keyboard shortcut handling
- `rubato` - Audio resampling (16kHz for Whisper)
- `rodio` - Audio playback for feedback sounds
- `enigo` - Keyboard simulation for text pasting
- `tauri-specta` - Type-safe bindings between Rust and TypeScript

**Frontend:**
- React with TypeScript
- Tailwind CSS for styling
- Zustand for state management
- Vite for bundling

### Important Platform-Specific Details

**macOS:**
- Uses Metal acceleration for Whisper models
- Requires accessibility permissions for global shortcuts and text pasting
- Uses NSPanel for overlay window
- Globe key support planned for future releases

**Windows:**
- Uses Vulkan acceleration for Whisper models
- Binaries are code-signed in CI/CD

**Linux:**
- OpenBLAS + Vulkan acceleration for Whisper models
- Overlay disabled by default (can steal focus on some compositors)
- Supports SIGUSR2 signal for recording toggle (useful for Wayland)
- Clipboard-based paste uses `wtype` or `dotool` on Wayland

### Settings System

Settings are stored using `tauri-plugin-store` in a JSON file at:
- macOS: `~/Library/Application Support/com.pais.handy/`
- Windows: `C:\Users\{username}\AppData\Roaming\com.pais.handy\`
- Linux: `~/.config/com.pais.handy/`

Key settings include:
- Keyboard shortcuts (with push-to-talk support)
- Audio device selection (input/output)
- Model preferences (Whisper Small/Medium/Turbo/Large, Parakeet V2/V3)
- VAD sensitivity
- Audio feedback options
- Language and translation settings
- Log levels

### Model Management

Models are downloaded on-demand and stored in the app data directory under `models/`:
- **Whisper models:** Single `.bin` files (ggml format)
- **Parakeet models:** Directories with ONNX model files extracted from `.tar.gz` archives

Models can be manually installed by placing files in the correct directory structure (see README.md "Manual Model Installation" section).

## Code Patterns and Conventions

### Rust Code Style

- Follow standard Rust formatting with `cargo fmt`
- Run `cargo clippy` and address warnings before committing
- Avoid `.unwrap()` in production code - use proper error handling with `anyhow::Result`
- Use `log::*` macros for logging (error, warn, info, debug, trace)
- Manager structs use `Arc<Mutex<T>>` for thread-safe shared state

### TypeScript/React Code Style

- Use TypeScript strictly - avoid `any` types
- Functional components only (no class components)
- Use React hooks for state and side effects
- Import types from `bindings.ts` for Rust type compatibility
- Use Tailwind CSS utility classes for styling

### Manager Pattern

Managers are initialized in `lib.rs` and registered with Tauri's state management:

```rust
let audio_manager = Arc::new(Mutex::new(AudioRecordingManager::new(app.clone())));
app.manage(audio_manager);
```

Commands access managers via Tauri's state API:

```rust
#[tauri::command]
fn some_command(manager: tauri::State<Arc<Mutex<SomeManager>>>) -> Result<()> {
    let manager = manager.lock().unwrap();
    // Use manager...
}
```

### Command Registration

Commands are defined in `commands/` modules and registered using `tauri-specta` for type-safe bindings:

```rust
Builder::<tauri::Wry>::new()
    .commands(collect_commands![
        commands::audio::some_command,
        // ...
    ])
```

This automatically generates TypeScript bindings in `src/bindings.ts`.

## Common Development Tasks

### Adding a New Setting

1. Add the field to the relevant struct in `src-tauri/src/settings.rs` (e.g., `AppSettings`)
2. Add default value in the struct's implementation
3. Frontend will automatically get the new type via `bindings.ts` after rebuild
4. Add UI component in `src/components/settings/` to expose the setting
5. Use the settings store hooks to read/write the setting

### Adding a New Tauri Command

1. Define the command function in appropriate `src-tauri/src/commands/*.rs` file
2. Add `#[tauri::command]` attribute and `#[specta::specta]` for type generation
3. Register command in the builder in `src-tauri/src/lib.rs`
4. Rebuild - TypeScript bindings will be auto-generated in `src/bindings.ts`
5. Import and use the command from the frontend via `import { invoke } from '@tauri-apps/api/core'`

### Working with Audio Processing

The audio pipeline is in `audio_toolkit/`:
- Recording uses `cpal` for cross-platform compatibility
- Audio is resampled to 16kHz mono for Whisper using `rubato`
- VAD uses Silero (ONNX model) to filter silence
- See `managers/audio.rs` for high-level orchestration

### Debugging

- Enable debug logging: Change log level in settings
- Access debug panel: `Cmd+Shift+D` (macOS) or `Ctrl+Shift+D` (Windows/Linux)
- Logs are written to both console and rotating log files
- Use `log::debug!`, `log::info!`, etc. in Rust code
- Use browser DevTools for frontend debugging (right-click → Inspect in dev mode)

## Known Issues and Caveats

### Whisper Model Crashes
- Whisper models crash on certain system configurations (Windows and Linux)
- Issue is configuration-dependent and not reproducible on all systems
- Parakeet models are a stable alternative that run CPU-only

### Linux Wayland Support
- Limited support for Wayland display server
- Overlay disabled by default (can steal focus)
- Use `wtype` or `dotool` for clipboard pasting on Wayland
- Can use SIGUSR2 signal for recording toggle from external keybind daemons

### Planned Refactorings
- Settings system refactoring (becoming bloated)
- Tauri commands cleanup (considering tauri-specta improvements)
- macOS keyboard shortcut handling rewrite
- Opt-in analytics implementation

## Build and Release

Production builds are created via GitHub Actions CI/CD:
- Multi-platform builds (macOS Intel/ARM, Windows x64/ARM64, Linux x64)
- Code signing on Windows and macOS
- Multiple Linux package formats (.deb, .rpm, AppImage)
- See `.github/workflows/` for build configuration

Manual production build: `bun run tauri build`

## Additional Resources

- **BUILD.md** - Detailed platform-specific build instructions
- **CONTRIBUTING.md** - Contribution guidelines and code style
- **AGENTS.md** - Similar guidance document (consider consolidating with this file)
- **README.md** - User-facing documentation and features
- [Discord Community](https://discord.com/invite/WVBeWsNXK4) - Developer discussions
