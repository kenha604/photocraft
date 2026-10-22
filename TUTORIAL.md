# KenShoppe tutorial

How to use what KenShoppe adds to PhotoCraft. What changed and when: [PATCHNOTES.md](PATCHNOTES.md).

- [Install or update](#install-or-update)
- [Generative Fill](#generative-fill)
  - [Set up Google Gemini](#set-up-google-gemini)
  - [Set up a local model (ComfyUI)](#set-up-a-local-model-comfyui)
  - [Fill a selection](#fill-a-selection)
  - [Art pass over the whole image](#art-pass-over-the-whole-image)
  - [Writing good prompts](#writing-good-prompts)
  - [Using your own ComfyUI workflow](#using-your-own-comfyui-workflow)
  - [Cost and privacy](#cost-and-privacy)
  - [Troubleshooting](#troubleshooting)
- [Keyboard](#keyboard)

---

## Install or update

1. **Close KenShoppe** (every window). Windows won't let a running app be replaced.
2. Double-click `install.py` in `C:\DEV_Photocraft`, or run it from a terminal in that folder:

   ```
   python install.py
   ```

3. Wait for `Installed to C:\Apps\Photocraft`. If it says **NOT INSTALLED … is in use**, KenShoppe is still open:
   close it and press Enter to retry.
4. Start KenShoppe from `C:\Apps\Photocraft\photocraft.exe`.

Use `python install.py --dest D:\Somewhere` to install elsewhere.

---

## Generative Fill

Repaint a selected area from a text description, or polish the whole image, with an AI image model. The model always
sees the full visible canvas (all layers, flattened), so it understands the scene.

### Set up Google Gemini

1. Get an API key at [aistudio.google.com/apikey](https://aistudio.google.com/apikey). Image generation usually
   needs billing turned on for your Google project.
2. In KenShoppe, open **Edit › Generative Fill…** (open any image first).
3. Paste the key into **Gemini API key** and click **Save key**.

The key is stored in **Windows Credential Manager**. It's never written to settings files or the repository, and
agents can't read it. To change or remove it, use **Change key** / **Remove key** in the same dialog. Alternatively,
set a `GEMINI_API_KEY` environment variable.

Pick the model in **Edit › Preferences › Integrations › Generative model**:

| Model | Use it for |
|---|---|
| Nano Banana 2.1 (default) | Everyday fills: fast, good quality |
| Nano Banana Pro | Final, highest-quality passes (costs the most) |
| Nano Banana 2 | The previous workhorse |
| Nano Banana 2 Lite | Quick, cheap experiments (1K output only) |
| Nano Banana | The original, legacy |

### Set up a local model (ComfyUI)

Runs on your own GPU; nothing leaves your computer.

1. Start **ComfyUI** (default address `http://127.0.0.1:8188`).
2. Install an **SDXL inpainting checkpoint** in `ComfyUI\models\checkpoints`, for example *Juggernaut XL Inpainting*.
   Any checkpoint with "inpaint" in its name is found automatically.
3. In KenShoppe: **Edit › Preferences › Integrations › Generative provider → ComfyUI**. Three more settings appear:
   - **ComfyUI address**: change it only if ComfyUI runs elsewhere.
   - **ComfyUI checkpoint**: leave empty to auto-pick an inpainting checkpoint, or type an exact file name.
   - **ComfyUI workflow file**: leave empty for the built-in SDXL Inpaint workflow (see
     [your own workflow](#using-your-own-comfyui-workflow)).

The first fill after starting ComfyUI is slower, because the model loads into VRAM. After that, a fill takes about
5–20 seconds on an RTX 3090.

### Fill a selection

1. **Select** the area to change with any selection tool. Select **a bit larger than the object**: the margin lets
   the model blend edges and add shadows. For softer blending, use **Select › Modify › Feather** with a few pixels.
2. Pick the layer the result should go into. It doesn't need to contain the pixels: the model reads the whole canvas.
3. **Edit › Generative Fill…**, or right-click the selection and choose **Generative Fill…**.
4. Type a prompt (see [Writing good prompts](#writing-good-prompts)).
5. **Paint into**:
   - **Active layer** (default) paints straight into the selected layer.
   - **New layer** keeps the result on its own layer, so you can mask it or delete it.
6. With ComfyUI, set **Strength**: 1.00 repaints the selection fully; lower keeps more of what's there.
7. Click **OK**. A progress bar shows while the model works; **Cancel** stops it, and nothing changes.

Only the selected pixels change, even if the model altered more. **Ctrl+Z** undoes the whole fill in one step.

### Art pass over the whole image

1. **Deselect** (Ctrl+D).
2. **Edit › Generative Fill…** and describe the polish, e.g. *"dramatic rim lighting from the right, deeper shadows,
   crisper detail on the ship"*.
3. With ComfyUI, start around **Strength 0.3–0.5**. Higher redraws more.
4. **OK**. The result lands on a new layer at the top (**Generative Art Pass**). Lower its opacity or mask it to mix
   it with your painting.

### Writing good prompts

**Gemini** understands instructions: *"change this headlight to be smashed"* works. **SDXL / FLUX (ComfyUI)** wants a
description of the result instead: *"smashed headlight, cracked glass, bent chrome rim"*.

Either way, detail about the result matters most:

| Vague | Better |
|---|---|
| make it damaged | cracked glass with a hole in the centre, bent chrome rim, dirt and scratches |
| make it metal | weathered steel hull plates with rivets, rust streaks under the seams |
| add a person | a woman in a red raincoat walking away from the viewer, small in the distance |

Tips:
- You don't need to say "only in the selection" or "match the style": KenShoppe already tells the model that.
- Describe what you want, not what to avoid: *"an empty, clean road"* beats *"no cars"*.
- Mention light and wear when they matter: *"lit by the streetlamp on the left"*, *"wet from rain"*.
- Iterate: undo, add one detail, run again. Click a **Recent** prompt in the dialog to start from it.

### Using your own ComfyUI workflow

Any ComfyUI workflow can drive Generative Fill, e.g. FLUX.1 Fill or Qwen-Image-Edit.

1. Build and test the workflow in ComfyUI. It should load an image and a mask, inpaint, and save the result.
2. **Rename (title) these nodes** exactly (right-click a node › Title):

   | Title | Node | KenShoppe fills in |
   |---|---|---|
   | `PC Prompt` | the positive text encoder (CLIP Text Encode) | your prompt |
   | `PC Image` | Load Image for the picture | the canvas |
   | `PC Mask` | Load Image for the mask | the selection mask (white = repaint) |
   | `PC Output` | Save Image | the result it downloads |
   | `PC Negative` *(optional)* | the negative text encoder | a default negative prompt |
   | `PC Sampler` *(optional)* | KSampler | `denoise` from Strength, and a fresh seed |

   For the mask, convert the loaded image to a mask with **Image To Mask** (channel red).
3. Export it with **Workflow › Export (API)**. The normal "Save" format isn't accepted.
4. In KenShoppe, set **Preferences › Integrations › ComfyUI workflow file** to the exported `.json` file's full path.

If something's missing, Generative Fill says which node it couldn't find.

### Cost and privacy

- **Gemini** charges per request, mainly by output resolution (1K or 2K tier), not by how big the selection is.
  KenShoppe never asks for 4K. Check [ai.google.dev/pricing](https://ai.google.dev/pricing), and see your actual
  spend in [AI Studio](https://aistudio.google.com). Nano Banana 2 Lite is the cheapest.
- **Gemini** receives the visible canvas and the selection mask; the dialog says so before you click OK.
- **ComfyUI** on your own computer is free and private.

### Troubleshooting

| Message | What to do |
|---|---|
| *needs an image model: set a Gemini API key…* | Save a key in the dialog, or switch the provider to ComfyUI |
| *Gemini rejected the API key* | Check the key in AI Studio; **Change key** in the dialog |
| *Gemini quota or rate limit reached* | Wait, or turn on billing for your Google project |
| *Gemini blocked the request* | Google's safety filter refused it; rephrase the prompt |
| *Could not reach ComfyUI … Is it running?* | Start ComfyUI, or fix **ComfyUI address** |
| *ComfyUI has no inpainting checkpoint installed* | Install one, or set **ComfyUI checkpoint** |
| *ComfyUI: KSampler: CUDA out of memory* | Close other GPU apps, or use a smaller model |
| *the workflow has no node titled "PC …"* | Title that node in ComfyUI and export again |

---

## Keyboard

- **Alt + mouse wheel** zooms. Alt alone never activates the menu bar.
  - **F10** puts the keyboard on the menu bar (← → to move, ↓ or Enter to open, Esc to leave).
  - **Alt + underlined letter** opens that menu (Alt+F for File, Alt+E for Edit, …).
- **Tool keys** select a tool and stay on it.
  - Pressing **B** again keeps the Brush.
  - From another tool, **B** brings back the brush-group tool you used last.
  - **Shift + key** cycles through a tool group: Shift+B goes Brush → Pencil → Mixer Brush → Color Replacement.
  - Hold a tool key for a temporary tool; let go to return to the previous one.
