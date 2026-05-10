#!/usr/bin/env python3
import argparse
import json
import sys


def render_partial(text: str, previous_width: int) -> int:
    if not sys.stdout.isatty():
        return previous_width

    line = f"partial: {text}"
    padding = " " * max(0, previous_width - len(line))
    print(f"\r{line}{padding}", end="", flush=True)
    return len(line)


def clear_partial(width: int) -> None:
    if sys.stdout.isatty() and width:
        print(f"\r{' ' * width}\r", end="", flush=True)


def print_final(text: str, partial_width: int) -> int:
    clear_partial(partial_width)
    print(f"final: {text}", flush=True)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Stream raw s16le PCM into Vosk.")
    parser.add_argument("--model", required=True)
    parser.add_argument("--rate", required=True, type=float)
    args = parser.parse_args()

    try:
        from vosk import KaldiRecognizer, Model
    except ImportError:
        print(
            "Vosk preview requires the Python 'vosk' package. Install it with: python3 -m pip install vosk",
            file=sys.stderr,
        )
        return 2

    model = Model(args.model)
    recognizer = KaldiRecognizer(model, args.rate)

    last_partial = ""
    partial_width = 0
    while True:
        chunk = sys.stdin.buffer.read(4096)
        if not chunk:
            break

        if recognizer.AcceptWaveform(chunk):
            result = json.loads(recognizer.Result())
            text = result.get("text", "").strip()
            if text:
                partial_width = print_final(text, partial_width)
            last_partial = ""
        else:
            result = json.loads(recognizer.PartialResult())
            partial = result.get("partial", "").strip()
            if partial and partial != last_partial:
                partial_width = render_partial(partial, partial_width)
                last_partial = partial

    result = json.loads(recognizer.FinalResult())
    text = result.get("text", "").strip()
    if text:
        print_final(text, partial_width)
    else:
        clear_partial(partial_width)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
