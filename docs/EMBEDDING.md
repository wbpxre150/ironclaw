# Local Embeddings with llama.cpp on GTX 1650 Mobile

## Model: mxbai-embed-large-v1

The highest-quality embedding model that fits on the GTX 1650 Mobile (4 GB VRAM).

| Metric | Value |
|--------|-------|
| MTEB Score | 64.7 |
| Dimensions | 1024 |
| Context | 512 tokens |
| VRAM (F16) | ~420 MB |
| License | Apache 2.0 |

Outperforms OpenAI `text-embedding-3-large` on the MTEB leaderboard.

---

## 1. Build llama.cpp with CUDA

```bash
# Install dependencies
sudo pacman -S --needed base-devel cmake git cuda nvidia-utils

# Clone and build targeting sm_75 (GTX 1650 Mobile = Turing, compute 7.5)
git clone https://github.com/ggml-org/llama.cpp
cd llama.cpp
cmake -B build \
  -DGGML_CUDA=ON \
  -DCMAKE_CUDA_ARCHITECTURES=75 \
  -DCMAKE_BUILD_TYPE=Release \
  -DLLAMA_CURL=ON
cmake --build build -j$(nproc)

# Add to PATH
echo 'export PATH=$PATH:'"$HOME/llama.cpp/build/bin" >> ~/.bashrc
source ~/.bashrc
```

If `nvcc` fails with a GCC version error:
```bash
sudo pacman -S gcc13
cmake -B build -DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=75 \
  -DCMAKE_C_COMPILER=gcc-13 -DCMAKE_CXX_COMPILER=g++-13 \
  -DCMAKE_BUILD_TYPE=Release
cmake --build build -j$(nproc)
```

---

## 2. Download the Model

```bash
pip install --user huggingface_hub
mkdir -p ~/models/embeddings

huggingface-cli download \
  ChristianAzinn/mxbai-embed-large-v1-gguf \
  mxbai-embed-large-v1-f16.GGUF \
  --local-dir ~/models/embeddings
```

The F16 file is ~420 MB and fits entirely in VRAM. Use `-ngl 99` to offload all layers.

---

## 3. Run the Embedding Server

```bash
llama-server \
  -m ~/models/embeddings/mxbai-embed-large-v1-f16.GGUF \
  -ngl 99 \
  --embeddings \
  --host 127.0.0.1 \
  --port 8080
```

Verify it works:
```bash
curl http://localhost:8080/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{
    "input": "Represent this sentence for searching relevant passages: What is machine learning?",
    "model": "mxbai-embed-large-v1"
  }'
```

> **Query prefix:** mxbai-embed-large-v1 requires the prefix
> `Represent this sentence for searching relevant passages:` on query strings.
> Documents being indexed need no prefix.

---

## 4. IronClaw Configuration

Replace `<chat-model-name>` with whatever model name your chat llama-server is
serving (e.g. the filename without `.gguf`, or whatever `/v1/models` returns).

### 4.1 LLM Backend (llama.cpp as chat model)

```bash
ironclaw config set llm_backend openai_compatible
ironclaw config set openai_compatible_base_url http://192.168.0.24:8081/v1
ironclaw config set selected_model <chat-model-name>
```

### 4.2 Embeddings (llama.cpp embedding server)

IronClaw has a dedicated `llamacpp` embedding provider. All settings are in the
settings store — no `.env` edits required.

```bash
ironclaw config set embeddings.enabled true
ironclaw config set embeddings.provider llamacpp
ironclaw config set embeddings.model mxbai-embed-large-v1
ironclaw config set embeddings.llamacpp_base_url http://192.168.0.24:8080
ironclaw config set embeddings.dimension 1024
```

The LLM backend (OpenRouter / `openai_compatible`) is completely independent and
is not affected by these settings.

---

## 5. Troubleshooting

**CUDA not found at build time:**
```bash
export PATH=/opt/cuda/bin:$PATH
export LD_LIBRARY_PATH=/opt/cuda/lib64:$LD_LIBRARY_PATH
```

**Out of memory:**
- The F16 model uses ~420 MB VRAM, well within 4 GB. If other processes are consuming
  VRAM, close them first or switch to Q8_0 (~220 MB):
  ```bash
  huggingface-cli download \
    ChristianAzinn/mxbai-embed-large-v1-gguf \
    mxbai-embed-large-v1-q8_0.GGUF \
    --local-dir ~/models/embeddings
  ```

**Wrong dimensions / search returning garbage:**
- Ensure `EMBEDDING_DIMENSION=1024` matches the model output.
- If you previously ran with a different model/dimension, the vector index in the
  database will be stale. Clear and re-index: `ironclaw memory reindex`.
