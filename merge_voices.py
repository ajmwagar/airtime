#!/usr/bin/env python3
"""Merge individual voice .bin files into a combined voices.bin file."""
import numpy as np
import os
import sys

def merge_voices(models_dir, output_path):
    """Merge all .bin files in models_dir into a single voices.bin."""
    voices = {}
    
    # Load the base voices.bin if it exists (look for voices_base.bin first)
    base_voices_path = os.path.join(models_dir, 'voices_base.bin')
    if not os.path.exists(base_voices_path):
        base_voices_path = os.path.join(models_dir, 'voices.bin')
    if os.path.exists(base_voices_path):
        print(f"Loading base voices from {base_voices_path}")
        base_data = np.load(base_voices_path)
        for voice_name in base_data.keys():
            voices[voice_name] = base_data[voice_name]
        print(f"  Loaded {len(base_data.keys())} voices from base file")
    
    # Load individual voice files
    for filename in sorted(os.listdir(models_dir)):
        if filename.endswith('.bin') and filename != 'voices.bin':
            voice_name = filename[:-4]  # Remove .bin extension
            if voice_name not in voices:
                filepath = os.path.join(models_dir, filename)
                try:
                    voice_data = np.fromfile(filepath, dtype=np.float32)
                    # Reshape based on the file size
                    # Individual voice files are (N, 256) where N is 510 or 511
                    if len(voice_data) % 256 == 0:
                        num_vectors = len(voice_data) // 256
                        voice_data = voice_data.reshape(num_vectors, 256)
                        # Add the extra dimension to make it (N, 1, 256)
                        voice_data = voice_data.reshape(num_vectors, 1, 256)
                        voices[voice_name] = voice_data
                        print(f"  Loaded {voice_name}: shape {voice_data.shape}")
                    else:
                        print(f"  Skipping {voice_name}: unexpected size {len(voice_data)} (not divisible by 256)")
                except Exception as e:
                    print(f"  Error loading {filename}: {e}")
    
    # Save merged voices
    if voices:
        np.savez(output_path, **voices)
        print(f"\nSaved {len(voices)} voices to {output_path}")
        print(f"Available voices: {sorted(voices.keys())}")
    else:
        print("No voices to save!")

if __name__ == "__main__":
    models_dir = sys.argv[1] if len(sys.argv) > 1 else "models"
    output_path = sys.argv[2] if len(sys.argv) > 2 else "models/voices.bin"
    merge_voices(models_dir, output_path)
