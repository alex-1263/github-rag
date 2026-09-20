"""Embedder: bge-m3 local CPU, fingerprint pinned into manifest."""
from __future__ import annotations

import hashlib
import os


class BgeM3Embedder:
    """Lazily-loaded local embedder. All callers share one instance per process."""

    def __init__(
        self,
        model: str = "BAAI/bge-m3",
        hf_mirror: bool = False,
        batch_size: int = 32,
        max_seq_len: int = 512,
    ):
        self.model_name = model
        self.batch_size = batch_size
        self.max_seq_len = max_seq_len
        self._hf_mirror = hf_mirror
        self._st = None  # lazy: 首次 embed 才加载模型

    # -- lifecycle ---------------------------------------------------------

    def _ensure_model(self):
        if self._st is not None:
            return
        if self._hf_mirror:
            os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
        from sentence_transformers import SentenceTransformer
        try:
            import torch

            torch.set_num_threads(max(os.cpu_count() or 8, 8))  # 吃满逻辑核
        except Exception:
            pass

        self._st = SentenceTransformer(self.model_name)
        # 关键性能项:bge-m3 默认 max_seq_length=8192,CPU 上按 8K 窗口算注意力
        # 会让吞吐跌一个数量级。512 是 bge-m3 的标准评测长度,质量几乎无损。
        self._st.max_seq_length = self.max_seq_len

    def fingerprint(self) -> str:
        """Environment pin: model + library version + seq len (manifest key)."""
        try:
            import sentence_transformers as st
            ver = st.__version__
        except Exception:
            ver = "unloaded"
        return f"{self.model_name}|st={ver}|len={self.max_seq_len}"

    # -- text assembly -----------------------------------------------------

    @staticmethod
    def build_text(
        title: str, body: str, title_repeats: int = 2, body_max_chars: int = 2000
    ) -> str:
        t = (title or "").strip()
        b = (body or "")[:body_max_chars]
        return (t + "\n") * title_repeats + b

    @staticmethod
    def text_hash(text: str) -> str:
        return hashlib.sha1(text.encode("utf-8")).hexdigest()

    # -- inference ---------------------------------------------------------

    def embed_texts(self, texts: list[str]) -> list[bytes]:
        self._ensure_model()
        vecs = self._st.encode(
            texts,
            batch_size=self.batch_size,
            normalize_embeddings=True,  # 归一化后余弦 = 点积
            show_progress_bar=len(texts) > 64,
        )
        import numpy as np

        return [v.astype(np.float32).tobytes() for v in vecs]

    def embed_query(self, text: str):
        self._ensure_model()
        import numpy as np

        v = self._st.encode([text], normalize_embeddings=True)[0]
        return v.astype(np.float32)
