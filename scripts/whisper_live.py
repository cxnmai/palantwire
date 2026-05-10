#!/usr/bin/env python3
import argparse
import re
import subprocess
import sys
import tempfile
import wave
from pathlib import Path


def write_wav(path: Path, pcm: bytes, rate: int) -> None:
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(pcm)


def clean_output(output: str) -> str:
    lines = []
    for line in output.splitlines():
        line = re.sub(r"^\s*\[[^\]]+\]\s*", "", line).strip()
        if line:
            lines.append(line)
    return " ".join(lines).strip()


def transcribe_chunk(whisper_cli: Path, model: Path, wav_path: Path) -> str:
    proc = subprocess.run(
        [
            str(whisper_cli),
            "--model",
            str(model),
            "--file",
            str(wav_path),
            "--language",
            "en",
            "--no-timestamps",
            "--no-prints",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return ""
    return clean_output(proc.stdout)


def emit_transcript(text: str, label: bool, previous_ended_sentence: bool) -> bool:
    if label:
        print(f"whisper: {text}", flush=True)
        return True

    text = re.sub(r"(?<=[.])\s+", "\n", text)

    if previous_ended_sentence:
        separator = "" if text.startswith("\n") else "\n"
    else:
        separator = " "

    if previous_ended_sentence is not None:
        print(separator, end="", flush=False)
    print(text, end="", flush=True)
    return text.rstrip().endswith(".")


def main() -> int:
    parser = argparse.ArgumentParser(description="Chunk raw s16le PCM into whisper.cpp.")
    parser.add_argument("--model", required=True, type=Path)
    parser.add_argument("--whisper-cli", required=True, type=Path)
    parser.add_argument("--rate", required=True, type=int)
    parser.add_argument("--chunk-seconds", type=int, default=5)
    parser.add_argument("--label", action="store_true")
    args = parser.parse_args()

    if not args.whisper_cli.exists():
        print(f"missing whisper-cli: {args.whisper_cli}", file=sys.stderr)
        return 2
    if not args.model.exists():
        print(f"missing Whisper model: {args.model}", file=sys.stderr)
        return 2

    chunk_bytes = args.rate * 2 * args.chunk_seconds
    pending = bytearray()
    index = 0
    previous_ended_sentence = None

    with tempfile.TemporaryDirectory(prefix="palantwire-whisper-") as temp_dir:
        temp_dir = Path(temp_dir)

        while True:
            chunk = sys.stdin.buffer.read(4096)
            if not chunk:
                break

            pending.extend(chunk)
            while len(pending) >= chunk_bytes:
                pcm = bytes(pending[:chunk_bytes])
                del pending[:chunk_bytes]

                wav_path = temp_dir / f"chunk-{index:06d}.wav"
                write_wav(wav_path, pcm, args.rate)
                text = transcribe_chunk(args.whisper_cli, args.model, wav_path)
                if text:
                    previous_ended_sentence = emit_transcript(
                        text, args.label, previous_ended_sentence
                    )
                index += 1

        if pending:
            wav_path = temp_dir / f"chunk-{index:06d}.wav"
            write_wav(wav_path, bytes(pending), args.rate)
            text = transcribe_chunk(args.whisper_cli, args.model, wav_path)
            if text:
                previous_ended_sentence = emit_transcript(
                    text, args.label, previous_ended_sentence
                )

    if previous_ended_sentence is not None and not args.label:
        print(flush=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
