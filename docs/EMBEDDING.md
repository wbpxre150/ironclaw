# Local Embeddings with llama.cpp on GTX 1650 Mobile

## Model: nomic-embed-text-v1.5

A high-quality embedding model with an 8192-token context window that fits easily on the GTX 1650 Mobile (4 GB VRAM).

| Metric | Value |
|--------|-------|
| MTEB Score | ~62.4 |
| Dimensions | 768 (Matryoshka: 64–768) |
| Context | 8192 tokens |
| VRAM (F16) | ~262 MB |
| License | Apache 2.0 |

Supports Matryoshka Representation Learning — you can use any dimension from 64 to 768 from a single model. The 8192-token context is a major advantage over mxbai-embed-large-v1's 512-token limit.

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
echo 'export PATH=$PATH:'"$HOME/llama.cpp/build/bin" >> ~/.zshrc
source ~/.zshrc
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
mkdir -p ~/models/embeddings
wget -O ~/models/embeddings/nomic-embed-text-v1.5-f16.gguf \
  https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/main/nomic-embed-text-v1.5.f16.gguf
```

The F16 file is ~262 MB and fits entirely in VRAM. Use `-ngl 99` to offload all layers.

---

## 3. Run the Embedding Server

```bash
llama-server \
  -m ~/models/embeddings/nomic-embed-text-v1.5-f16.gguf \
  -ngl 99 \
  --embeddings \
  -c 8192 \
  --rope-scaling yarn \
  --rope-freq-scale .75 \
  --host 127.0.0.1 \
  --port 8080
```

The `--rope-scaling yarn --rope-freq-scale .75` flags are required to utilize the full 8192-token context window.

Verify it works:
```bash
curl http://localhost:8080/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{
    "input": "search_query: What is machine learning?",
    "model": "nomic-embed-text-v1.5"
  }'
```

> **Task instruction prefixes:** nomic-embed-text-v1.5 uses task-type prefixes.
> - Queries: `search_query: <text>`
> - Documents being indexed: `search_document: <text>`
> - Classification: `classification: <text>`
> - Clustering: `clustering: <text>`
>
> For RAG, use `search_query:` on queries and `search_document:` on indexed passages.

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
ironclaw config set embeddings.model nomic-embed-text-v1.5
ironclaw config set embeddings.llamacpp_base_url http://192.168.0.24:8080
ironclaw config set embeddings.dimension 768
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
- The F16 model uses ~262 MB VRAM, well within 4 GB. If other processes are consuming
  VRAM, close them first or switch to Q8_0 (~140 MB):
  ```bash
  wget -O ~/models/embeddings/nomic-embed-text-v1.5-f16.gguf \
    https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/main/nomic-embed-text-v1.5.Q8_0.gguf
  ```
  The filename you save it as is what `-m` points to, so keeping the name consistent means no other
  commands need to change.

**Wrong dimensions / search returning garbage:**
- Ensure `embeddings.dimension` is set to `768` (or your chosen Matryoshka dimension).
- If you previously ran with a different model/dimension, the vector index in the
  database will be stale. Clear and re-index: `ironclaw memory reindex`.

**Context truncation:**
- Without `--rope-scaling yarn --rope-freq-scale .75`, the server defaults to a
  shorter context. Always pass those flags when starting the server to get the
  full 8192-token window.
