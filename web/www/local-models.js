// Curated catalog of local GGUF models runnable in-browser via wllama. These
// are not user-configurable providers/models — they back the built-in
// "Local (in-browser)" provider rendered by settings.js.
export const LOCAL_PROVIDER_ID = "local";

export const LOCAL_MODEL_CATALOG = [
  {
    id: "lfm2.5-230m-q8",
    name: "LFM2.5-230M (Q8_0, 247 MB)",
    url: "https://huggingface.co/LiquidAI/LFM2.5-230M-GGUF/resolve/main/LFM2.5-230M-Q8_0.gguf",
  },
  {
    id: "lfm2.5-230m-q4",
    name: "LFM2.5-230M (Q4_K_M, 153 MB)",
    url: "https://huggingface.co/LiquidAI/LFM2.5-230M-GGUF/resolve/main/LFM2.5-230M-Q4_K_M.gguf",
  },
  {
    id: "qwen3-0.6b-q8",
    name: "Qwen3-0.6B (Q8_0, 639 MB)",
    url: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q8_0.gguf",
  },
];
