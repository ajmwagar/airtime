# TTS Setup Guide

Airtime uses [Kokoro](https://github.com/hexgrad/kokoro) for text-to-speech via the `kokoro-onnx` Python library.

## Quick Start

1. Install dependencies:
```bash
pip3 install kokoro-onnx soundfile numpy --break-system-packages
```

2. Download the model files (already in repo):
- `models/kokoro-v1.0.onnx` - The ONNX model
- `models/voices.bin` - Voice embeddings

3. Test TTS:
```bash
echo "Hello from Airtime" | python3 docker/kokoro \
  --model models/kokoro-v1.0.onnx \
  --voices models/voices.bin \
  --voice af_bella \
  --out /tmp/test.wav
```

## Voice Files

The `voices.bin` file is a NumPy .npz archive containing voice embeddings. Available voices:

| Voice | Gender | Accent | Notes |
|-------|--------|--------|-------|
| `af` | Female | American | Default female voice |
| `af_bella` | Female | American | Smooth, warm - good for jazz |
| `af_nicole` | Female | American | Neutral, clear |
| `af_sarah` | Female | American | Pleasant |
| `af_sky` | Female | American | Clear |
| `am_adam` | Male | American | - |
| `am_michael` | Male | American | Good for GTA-style stations |
| `bf_emma` | Female | British | - |
| `bf_isabella` | Female | British | - |
| `bm_george` | Male | British | - |
| `bm_lewis` | Male | British | - |

## Adding Voices

To add a voice (e.g., `af_heart`):

```bash
cd models
wget https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/voices/af_heart.bin
```

Then merge into voices.bin:
```bash
cd ..
python3 merge_voices.py
```

**Note**: Some voices from HuggingFace have 510 style vectors instead of 511, which can cause shape mismatches. Stick to the voices in the base `voices.bin` for best compatibility.

## Troubleshooting

### "Voice not found"
- Check `models/voices.bin` exists and contains the voice
- Run `python3 -c "import numpy as np; print(np.load('models/voices.bin').keys())"`

### "Unexpected input data type" error
This is a `kokoro-onnx` library bug (v0.5.0). The speed parameter is set to int32 but the model expects float32.

**Fix**: Edit `/home/ajmwagar/.local/lib/python3.11/site-packages/kokoro_onnx/__init__.py` line 115:
```python
# Change:
"speed": np.array([speed], dtype=np.int32),
# To:
"speed": np.array([speed], dtype=np.float32),
```

### Audio format issues
If you get "Format not recognised" errors, the audio samples may be 2D. The `docker/kokoro` shim automatically flattens them.

## Configuration

Set in `settings.toml`:
```toml
[kokoro]
model_path  = "./models/kokoro-v1.0.onnx"
voices_path = "./models/voices.bin"
binary      = "./docker/kokoro"  # Path to kokoro shim
```

Per-persona voice in `personas/*.toml`:
```toml
[host]
voice_model = "af_bella"
```
