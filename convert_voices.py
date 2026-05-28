#!/usr/bin/env python3
"""Convert voices.json to voices.bin (NumPy .npz format) for kokoro-onnx."""
import json
import numpy as np
import sys

def convert_json_to_bin(json_path, bin_path):
    """Convert voices.json to voices.bin (NumPy .npz format)."""
    with open(json_path, 'r') as f:
        voices = json.load(f)
    
    # Convert to numpy arrays
    np_voices = {}
    for voice_name, voice_data in voices.items():
        # Handle nested structure - voices.json has format {"af": [[...]], "am": [[...]]}
        # where each voice has multiple style vectors
        if isinstance(voice_data, list):
            np_voices[voice_name] = np.array(voice_data, dtype=np.float32)
        else:
            print(f"Warning: unexpected format for voice {voice_name}")
    
    # Save as .npz (which is what kokoro-onnx expects for voices.bin)
    np.savez(bin_path, **np_voices)
    print(f"Converted {len(np_voices)} voices from {json_path} to {bin_path}")

if __name__ == "__main__":
    json_path = sys.argv[1] if len(sys.argv) > 1 else "models/voices.json"
    bin_path = sys.argv[2] if len(sys.argv) > 2 else "models/voices.bin"
    convert_json_to_bin(json_path, bin_path)
