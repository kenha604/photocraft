# KenShoppe patch notes

KenShoppe is a custom build of [PhotoCraft](https://github.com/storytold/photocraft) on the `custom` branch of
[kenha604/photocraft](https://github.com/kenha604/photocraft). These notes cover what it changes compared with
upstream PhotoCraft; upstream's own changes come in when `main` is merged into `custom`. Newest first.

How to use the new features: [TUTORIAL.md](TUTORIAL.md).

---

## 2026-10-22

### New: Generative Fill (Edit › Generative Fill…)

Select an area, describe what it should become, and an AI model repaints just that area, matching the rest of the
picture. With nothing selected it does an **art pass** over the whole image (polish, lighting, style).

- The model sees the **whole visible canvas**, every layer flattened, not just the active layer, so it understands
  what you're painting.
- The result is painted **into the active layer** inside the selection only. Feathered edges blend, pixels outside
  the selection never change, and **one Ctrl+Z** undoes it. Choose **New layer** in the dialog to keep it separate.
- An art pass goes onto a new layer at the top of the stack.
- It runs in the background with a progress bar and **Cancel**. The dialog remembers your recent prompts.
- Also in the selection's right-click menu.

Two model providers, picked in **Edit › Preferences › Integrations › Generative provider**:

- **Google Gemini** (cloud, "Nano Banana" image models). Paste your API key into the Generative Fill dialog once. It
  is stored in **Windows Credential Manager**, never in settings files or the repository. Pick the model under
  **Generative model**: Nano Banana 2.1 (default), Pro, 2, 2 Lite or the original.
- **ComfyUI** (local, your own GPU, nothing leaves your computer).
  - The built-in **SDXL Inpaint** workflow finds an installed inpainting checkpoint automatically (e.g. Juggernaut XL
    Inpainting).
  - Use any workflow of your own, exported from ComfyUI in API format.
  - A **Strength** slider: 1.0 repaints fully, about 0.3–0.5 polishes without redrawing.

When the selection is small on a big canvas, the area around it is sent at higher resolution together with a
thumbnail of the whole canvas, so small fixes keep their detail.

### Changed: keyboard

- **Alt no longer activates the menu bar.** Tapping Alt (including after Alt + mouse-wheel zoom) leaves the menus
  alone. **F10** still puts the keyboard on the menu bar, and **Alt + letter** (Alt+F for File, …) still opens a menu.
- **Tool keys no longer cycle.** Pressing a tool's key again keeps that tool. From another tool, the key brings back
  whichever tool of that group you used last. **Shift + key** cycles through the group (B → Pencil → Mixer Brush …).
  This applies whatever **Preferences › Tools › Use Shift Key for Tool Switch** is set to.

### Changed: installing

- `install.py` replaces `install.ps1`. Double-click it, or run `python install.py`. It builds the app and copies it to
  `C:\Apps\Photocraft` (`--dest` for another folder).
- If KenShoppe is open, so the old copy can't be replaced, it now says so clearly and waits for you to close it,
  instead of quietly leaving the old version installed.

### Upstream

- Merged the latest PhotoCraft `main` (899 commits), including spring-loaded tools (hold a tool key for a temporary
  tool), the dockable Brushes panel, single-instance launch and PDF save.

---

## 2026-10-17

- **V keeps the Move tool**: pressing V again no longer switches to the Artboard tool. Now part of the general "tool
  keys don't cycle" rule above.
- **Splash screen** on start-up, rotating through your own launch artwork. The images in `assets/splash/` stay on
  your computer only: they are git-ignored and never uploaded.
- **Windows pen tablets**: pen tilt, rotation and the eraser end are read directly from Windows (`WM_POINTER`), which
  the standard window library drops.
- **KenShoppe branding**: window title, About box, app icon and file-type names.
- **Resizable Brush Settings panel**: drag its edges; the controls and the preset list follow the size.

## 2026-10-11

- Added a local rebuild-and-install script (now `install.py`).
