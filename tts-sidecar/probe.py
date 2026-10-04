"""Drives a running sidecar: /info, a three-chunk stream saved as WAV with latency and RTF, then a cancel."""

import argparse
import asyncio
import json
import time
import wave

import aiohttp

CHUNKS = [
    "Hi, it's Note.",
    "I moved your run to tomorrow at seven, and the dentist is still on Thursday.",
    "Do you want me to remove all three practice tests, or just the first one?",
]
LONG = "This is a long sentence that keeps going so that there is plenty of time to cancel it while it renders. " * 2


async def wait_ready(session, base):
    t0 = time.perf_counter()
    while True:
        try:
            async with session.get(f"{base}/info") as r:
                info = await r.json()
            if info.get("ready"):
                print(f"info after {time.perf_counter() - t0:.1f} s:", json.dumps(info))
                return info
        except aiohttp.ClientConnectionError:
            pass
        await asyncio.sleep(1)


async def speak(session, base, voice, out):
    async with session.ws_connect(f"{base}/stream") as ws:
        t0 = time.perf_counter()
        await ws.send_json({"type": "open", "voice": voice})
        for c in CHUNKS:
            await ws.send_json({"type": "text", "text": c})
        await ws.send_json({"type": "end"})
        pcm, first, marks = bytearray(), None, []
        async for msg in ws:
            if msg.type == aiohttp.WSMsgType.BINARY:
                first = first or time.perf_counter() - t0
                pcm += msg.data
            elif msg.type == aiohttp.WSMsgType.TEXT:
                m = json.loads(msg.data)
                if m["type"] == "mark":
                    marks.append((m["chars"], round(time.perf_counter() - t0, 2), len(pcm) // 2))
                elif m["type"] == "done":
                    break
                else:
                    raise SystemExit(f"unexpected {m}")
        total = time.perf_counter() - t0
    audio_s = len(pcm) / 2 / 24000
    assert [m[0] for m in marks] == [len(c) for c in CHUNKS], marks
    print(f"voice {voice}: first audio {first:.2f} s, total {total:.2f} s, audio {audio_s:.2f} s, RTF {total / audio_s:.2f}")
    print("  marks (chars, at s, samples):", marks)
    if out:
        with wave.open(out, "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(24000)
            w.writeframes(bytes(pcm))
        print("  wrote", out)


async def cancel(session, base):
    async with session.ws_connect(f"{base}/stream") as ws:
        await ws.send_json({"type": "open", "voice": "default"})
        await ws.send_json({"type": "text", "text": LONG})
        await ws.send_json({"type": "text", "text": LONG})
        await asyncio.sleep(1.0)
        t0 = time.perf_counter()
        await ws.send_json({"type": "cancel"})
        after = []
        async for msg in ws:
            after.append(msg.type.name)
        print(f"cancel: closed {time.perf_counter() - t0:.2f} s after cancel, frames after cancel: {after or 'none'}")
    t0 = time.perf_counter()
    async with session.ws_connect(f"{base}/stream") as ws:
        await ws.send_json({"type": "open", "voice": "default"})
        await ws.send_json({"type": "text", "text": "Got it."})
        await ws.send_json({"type": "end"})
        async for msg in ws:
            if msg.type == aiohttp.WSMsgType.BINARY:
                print(f"next stream's first audio {time.perf_counter() - t0:.2f} s after the cancel")
                break


async def main():
    p = argparse.ArgumentParser()
    p.add_argument("--url", default="http://127.0.0.1:8890")
    p.add_argument("--voice", action="append")
    p.add_argument("--out")
    a = p.parse_args()
    async with aiohttp.ClientSession() as session:
        info = await wait_ready(session, a.url)
        voices = a.voice or [v["id"] for v in info["voices"]]
        for i, v in enumerate(voices):
            await speak(session, a.url, v, a.out.replace(".wav", f"-{v}.wav") if a.out else None)
        await cancel(session, a.url)


asyncio.run(main())
