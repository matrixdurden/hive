"""engram'ın yerel sesi: XTTS v2 (ekran kartında), yalnızca 127.0.0.1'den.

engram açılınca WSL'de bunu başlatır ve stdin'i açık tutar; engram kapanınca stdin kapanır,
sunucu da kapanır. Kurulum: `make ses-kur` (depo kökünde).

  GET  /saglik              -> 200 "hazir" (model yüklendiyse), yoksa 503
  POST /konus  {"text", "speaker", "speed"}
                            -> 24 kHz mono s16le ham ses, üretildikçe akar (bağlantı sonunda biter)
  GET  /sesler              -> hazır seslerin adları, satır satır
  POST /kim   (16 kHz mono s16le ham ses) -> {"benzerlik": 0..1, "kayit": n}: kullanıcının sesine ne kadar benziyor
  POST /tani  (aynı biçim)  -> {"kayit": n}: kullanıcının sesinden bir örnek daha öğren
  POST /unut                -> öğrenilmiş ses izini siler

Konuşmacı tanıma SpeechBrain ECAPA (speechbrain/spkrec-ecapa-voxceleb) ile, ekran kartında.

XTTS v2 lisansı (Coqui Public Model License) yalnızca ticari olmayan kullanıma izin verir.
"""

import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

os.environ["COQUI_TOS_AGREED"] = "1"

PORT = int(os.environ.get("ENGRAM_SES_PORT", "5123"))
STATE = {"model": None, "ecapa": None}
VOICEPRINT = os.path.expanduser("~/.local/share/engram-xtts/ben.pt")
PRINTS = []
GPU = threading.Lock()
# Ses tanıma konuşma üretimini beklemesin (söz kesme anında doğrulanır): ayrı kilit.
EAR = threading.Lock()


def load():
    import torch
    from TTS.api import TTS


    tts = TTS("tts_models/multilingual/multi-dataset/xtts_v2").to("cuda" if torch.cuda.is_available() else "cpu")
    model = tts.synthesizer.tts_model
    # İlk çıkarım yavaştır: ısıt.
    d = next(iter(model.speaker_manager.speakers.values()))
    model.inference("Hazır.", "tr", d["gpt_cond_latent"], d["speaker_embedding"])
    STATE["model"] = model
    print("xtts hazır", flush=True)
    try:
        from speechbrain.inference.speaker import EncoderClassifier

        STATE["ecapa"] = EncoderClassifier.from_hparams(
            source="speechbrain/spkrec-ecapa-voxceleb",
            savedir=os.path.expanduser("~/.local/share/engram-xtts/ecapa"),
            run_opts={"device": "cuda:0" if torch.cuda.is_available() else "cpu"},
        )
        if os.path.exists(VOICEPRINT):
            PRINTS.extend(torch.load(VOICEPRINT))
        print(f"ses tanıma hazır ({len(PRINTS)} örnek)", flush=True)
    except Exception as e:
        print(f"ses tanıma yüklenemedi: {e}", flush=True)


def embed(raw: bytes):
    """16 kHz mono s16le → birim uzunlukta ses izi."""
    import numpy as np
    import torch

    w = torch.from_numpy(np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0).unsqueeze(0)
    with EAR, torch.no_grad():
        e = STATE["ecapa"].encode_batch(w.to(STATE["ecapa"].device)).squeeze().float().cpu()
    return torch.nn.functional.normalize(e, dim=0)


def reference():
    """Örneklerin ortalaması; ortalamaya hiç benzemeyen (gürültü, başkası) örnekler dışarıda."""
    import torch

    keep = list(PRINTS)
    for _ in range(2):
        ref = torch.nn.functional.normalize(torch.stack(keep).mean(0), dim=0)
        if len(keep) <= 3:
            break
        kept = [e for e in keep if float(e @ ref) >= 0.3]
        if len(kept) < 3 or len(kept) == len(keep):
            break
        keep = kept
    return ref


def watch_stdin():
    """engram kapanınca (stdin kapanır) sunucu da kapanır."""
    try:
        while sys.stdin.buffer.read(1):
            pass
    except Exception:
        pass
    os._exit(0)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def do_GET(self):
        model = STATE["model"]
        if self.path == "/saglik":
            self.send_response(200 if model else 503)
            self.end_headers()
            self.wfile.write(f"hazir {len(PRINTS)}".encode() if model else b"yukleniyor")
        elif self.path == "/sesler" and model:
            self.send_response(200)
            self.send_header("content-type", "text/plain; charset=utf-8")
            self.end_headers()
            self.wfile.write("\n".join(model.speaker_manager.speakers.keys()).encode())
        else:
            self.send_response(404)
            self.end_headers()

    def reply_json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path in ("/kim", "/tani", "/unut"):
            if STATE["ecapa"] is None:
                return self.reply_json({"hata": "ses tanıma hazır değil"}, 503)
            raw = self.rfile.read(int(self.headers.get("content-length", "0")))
            import torch

            if self.path == "/unut":
                PRINTS.clear()
                if os.path.exists(VOICEPRINT):
                    os.remove(VOICEPRINT)
                return self.reply_json({"kayit": 0})
            if len(raw) < 16000:  # 0,5 sn'den kısa
                return self.reply_json({"hata": "çok kısa"}, 400)
            e = embed(raw)
            if self.path == "/tani":
                PRINTS.append(e)
                del PRINTS[:-24]
                torch.save(PRINTS, VOICEPRINT)
                return self.reply_json({"kayit": len(PRINTS)})
            if not PRINTS:
                return self.reply_json({"benzerlik": 1.0, "kayit": 0})
            return self.reply_json({"benzerlik": round(float(e @ reference()), 3), "kayit": len(PRINTS)})
        model = STATE["model"]
        if self.path != "/konus" or not model:
            self.send_response(503 if model is None else 404)
            self.end_headers()
            return
        try:
            req = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))) or b"{}")
            text = str(req.get("text", "")).strip()
            name = req.get("speaker") or "Suad Qasim"
            speed = float(req.get("speed") or 0.92)
            spk = model.speaker_manager.speakers.get(name) or next(iter(model.speaker_manager.speakers.values()))
        except Exception as e:
            self.send_response(400)
            self.end_headers()
            self.wfile.write(str(e).encode())
            return
        self.send_response(200)
        self.send_header("content-type", "audio/L16; rate=24000; channels=1")
        self.end_headers()
        if not text:
            return
        import numpy as np

        with GPU:
            try:
                for chunk in model.inference_stream(
                    text,
                    "tr",
                    spk["gpt_cond_latent"],
                    spk["speaker_embedding"],
                    stream_chunk_size=20,
                    temperature=0.45,
                    repetition_penalty=5.0,
                    speed=speed,
                    enable_text_splitting=True,
                ):
                    pcm = (chunk.clamp(-1, 1).cpu().numpy() * 32767).astype(np.int16).tobytes()
                    self.wfile.write(pcm)
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                # engram konuşmayı kesti.
                pass


def main():
    threading.Thread(target=watch_stdin, daemon=True).start()
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    print(f"xtts dinliyor :{PORT}, model yükleniyor", flush=True)
    load()
    threading.Event().wait()


if __name__ == "__main__":
    main()
