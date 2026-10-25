---
license: apache-2.0
language:
- en
base_model:
- ibm-granite/granite-embedding-english-r2
pipeline_tag: text-ranking
library_name: sentence-transformers
tags:
- granite
- transformers
- embeddings
- mteb
- text-embeddings-inference
---

# granite-embedding-reranker-english-r2
<!-- Provide a quick summary of what the model is/does. -->

**Model Summary:** _granite-embedding-reranker-english-r2_ is a 149M parameter dense cross-encoder model from the Granite Embeddings collection that can be used to generate high quality text embeddings. This model produces embedding vectors of size 768 based on context length of upto 8192 tokens. Compared to most other open-source models, this model was only trained using open-source relevance-pair datasets with permissive, enterprise-friendly license, plus IBM collected and generated datasets. 

The _granite-embedding-reranker-english-r2_ model uses a cross-encoder architecture to compute high-quality relevance scores between queries and documents by jointly encoding their text, enabling precise reranking based on contextual alignment. 
The model is trained with ranking-specific loss functions such as pListMLE, along with model merging techniques to enhance performance. The reranker model shows strong performance on standard information retrieval benchmark (BEIR, MIRACL), long-document search benchmarks (MLDR), and on many enterprise use cases.

The latest granite embedding r2 release introduces two English embedding models, and one English reranking all based on the ModernBERT architecture:
- _granite-embedding-english-r2_ (**149M** parameters): with an output embedding size of _768_, replacing _granite-embedding-125m-english_. 
- _granite-embedding-small-english-r2_ (**47M** parameters): A _first-of-its-kind_ reduced-size model, with 8192 context length support, fewer layers and a smaller output embedding size (_384_), replacing _granite-embedding-30m-english_.
- **_granite-embedding-reranker-english-r2_** (**149M** parameters): reranker model based on _granite-embedding-english-r2_, with an output embedding size of _768_.

## Model Details

- **Developed by:** Granite Embedding Team, IBM
- **Repository:** [ibm-granite/granite-embedding-models](https://github.com/ibm-granite/granite-embedding-models)
- **Paper:** [Granite Embedding R2 Models](https://arxiv.org/abs/2508.21085)
- **Language(s) (NLP):** English
- **Release Date**: Sep 8, 2025
- **License:** [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0)

## Usage

<!-- Address questions around how the model is intended to be used, including the foreseeable users of the model and those affected by the model. -->
The model is designed to compute relevance scores for query-document pairs, making it well-suited for reranking tasks in information retrieval and search applications.

**Usage with Sentence Transformers:** 
The model is compatible with SentenceTransformer library and is very easy to use:

First, install the sentence transformers library
```shell
pip install sentence_transformers
```

The model can then be used to jointly encode pairs of text to compute a relevance score.

```python
from sentence_transformers import CrossEncoder, util

model_path = "ibm-granite/granite-embedding-reranker-english-r2"
# Load the Sentence Transformer model
model = CrossEncoder(model_path)

passages = [
               "Romeo and Juliet is a play by William Shakespeare.",
               "Climate change refers to long-term shifts in temperatures.",
               "Shakespeare also wrote Hamlet and Macbeth.",
               "Water is an inorganic compound with the chemical formula H2O.",
               "In liquid form, H2O is also called 'water' at standard temperature and pressure."
            ]

query = "what is the chemical formula of water?"

# encodes query and passages jointly and computes relevance score.
ranks = model.rank(query, passages, return_documents=True)

# Print document rank and relevance score
for rank in ranks:
    print(f"- #{rank['corpus_id']} ({rank['score']:.2f}): {rank['text']}")
```

**Usage with Huggingface Transformers:** 
This is a simple example of how to use the reranking model with the Transformers library and PyTorch.

First, install the required libraries
```shell
pip install transformers torch
```

The model can then be used to encode pairs of text

```python
import torch
from transformers import AutoModelForSequenceClassification, AutoTokenizer

model_path = "ibm-granite/granite-embedding-reranker-english-r2"

# Load the model and tokenizer
model = AutoModelForSequenceClassification.from_pretrained(model_path).eval()
tokenizer = AutoTokenizer.from_pretrained(model_path)

pairs = [
    ["what is the chemical formula of water?", "Water is an inorganic compound with the chemical formula H2O."],
    ["what is the chemical formula of water?", "In liquid form, H2O is also called 'water' at standard temperature and pressure."],
    ["how to implement quick sort in python?", "The weather is nice today"],
]


# tokenize inputs
tokenized_pairs = tokenizer(pairs, padding=True, truncation=True, return_tensors='pt')

# encode and compute scores
with torch.no_grad():
    scores = model(**tokenized_pairs, return_dict=True).logits.view(-1, ).float()
    print(scores)

```

**Usage with Huggingface Transformers (Retriever + Reranker E2E):**
This is a simple example of how to use the Granite retriever and reranker together end-to-end with the Transformers library and PyTorch. The retriever first finds the most relevant candidate documents for a query, and then the reranker re-orders those candidates to produce the final ranked list.

```python
import torch
from transformers import AutoModel, AutoTokenizer, AutoModelForSequenceClassification

# --------------------------
# 1. Load retriever (149M)
# --------------------------
retriever_model_path = "ibm-granite/granite-embedding-english-r2"
retriever = AutoModel.from_pretrained(retriever_model_path).eval()
retriever_tokenizer = AutoTokenizer.from_pretrained(retriever_model_path)

# Example query + candidate documents
query = "what is the chemical formula of water?"
documents = [
    "Water is an inorganic compound with the chemical formula H2O.",
    "In liquid form, H2O is also called 'water' at standard temperature and pressure.",
    "The weather is nice today",
    "Quick sort is a divide and conquer algorithm that sorts by partitioning."
]

# Encode query and documents
with torch.no_grad():
    query_emb = retriever(
        **retriever_tokenizer(query, return_tensors="pt", truncation=True, padding=True)
    ).last_hidden_state[:, 0, :]   # CLS embedding

    doc_embs = retriever(
        **retriever_tokenizer(documents, return_tensors="pt", truncation=True, padding=True)
    ).last_hidden_state[:, 0, :]

# Compute cosine similarity
query_emb = torch.nn.functional.normalize(query_emb, dim=-1)
doc_embs = torch.nn.functional.normalize(doc_embs, dim=-1)
similarities = torch.matmul(query_emb, doc_embs.T).squeeze(0)

# Rank docs by retriever
retriever_ranked = sorted(
    zip(documents, similarities.tolist()),
    key=lambda x: x[1],
    reverse=True
)
print("Retriever ranking:")
for doc, score in retriever_ranked:
    print(f"{score:.4f} | {doc}")


# --------------------------
# 2. Load reranker (149M)
# --------------------------
reranker_model_path = "ibm-granite/granite-embedding-reranker-english-r2"
reranker = AutoModelForSequenceClassification.from_pretrained(reranker_model_path).eval()
reranker_tokenizer = AutoTokenizer.from_pretrained(reranker_model_path)

# Prepare top-k candidates (say top 3 from retriever)
top_k = 3
candidate_pairs = [[query, doc] for doc, _ in retriever_ranked[:top_k]]

# Tokenize and rerank
with torch.no_grad():
    tokenized_pairs = reranker_tokenizer(
        candidate_pairs, padding=True, truncation=True, return_tensors="pt"
    )
    rerank_scores = reranker(**tokenized_pairs).logits.view(-1, ).float()

# Rank docs by reranker
reranker_ranked = sorted(
    zip([doc for doc, _ in retriever_ranked[:top_k]], rerank_scores.tolist()),
    key=lambda x: x[1],
    reverse=True
)

print("\nReranker final ranking:")
for doc, score in reranker_ranked:
    print(f"{score:.4f} | {doc}")
```

**Usage with Hugging Face Text Embeddings Inference (TEI):** 

This is a simple example of how to deploy the reranking model with [Text Embeddings Inference (TEI)](https://github.com/huggingface/text-embeddings-inference), a blazing fast inference solution for text embedding models, via Docker.

- On CPU:
```bash
docker run -p 8080:80 -v hf_cache:/data --pull always ghcr.io/huggingface/text-embeddings-inference:cpu-latest --model-id ibm-granite/granite-embedding-reranker-english-r2
```

- On NVIDIA GPU:
```bash
docker run --gpus all -p 8080:80 -v hf_cache:/data --pull always ghcr.io/huggingface/text-embeddings-inference:cuda-latest --model-id ibm-granite/granite-embedding-reranker-english-r2
```

Then you can send requests to the deployed API via the `/rerank` route (see the [Text Embeddings Inference OpenAPI Specification](https://huggingface.github.io/text-embeddings-inference/) for more details):

```bash
curl http://0.0.0.0:8080/rerank \
  -H "Content-Type: application/json" \
  -d '{
    "query": "what is the chemical formula of water?",
    "texts": [
      "Water is an inorganic compound with the chemical formula H2O.",
      "In liquid form, H2O is also called '\''water'\'' at standard temperature and pressure.",
      "The weather is nice today",
      "Quick sort is a divide and conquer algorithm that sorts by partitioning."
    ],
    "raw_scores": false,
    "return_text": false,
    "truncate": true,
    "truncation_direction": "Right"
  }'
```

## Evaluation Results

The performance of the Granite Embedding English reranking model on BEIR, MLDR, and Miracl benchmarks is reported below. All models are evaluated on the top-20 documents retrieved from the granite-embedding-english-small-r2 or granite-embedding-english-r2 retrievers respectively. 
Each reranking model is evaluated with its maximum supported sequence length, while queries are truncated to 64 tokens.

| Model                                                              | Parameters (M) | Seq. Length | BEIR Avg. | MLDR (en) | Miracl (en) |
|--------------------------------------------------------------------|----------------|-------------|-----------|-----------|-------------|
| **Retriever: granite-embedding-small-english-r2**                  | 47             | 8192        | 50.9      | 40.1      | 42.4        |
| &nbsp;&nbsp;&nbsp;&nbsp;ms-marco-MiniLM-L12-v2                     | 33             | 512         | 52.0      | 34.8      | 54.5        |
| &nbsp;&nbsp;&nbsp;&nbsp;bge-reranker-base                          | 278            | 512         | 51.6      | 36.7      | 40.7        |
| &nbsp;&nbsp;&nbsp;&nbsp;bge-reranker-large                         | 560            | 512         | 53.0      | 37.9      | 42.2        |
| &nbsp;&nbsp;&nbsp;&nbsp;gte-reranker-modernbert-base               | 149            | 8192        | 54.8      | 50.4      | 54.3        |
| &nbsp;&nbsp;&nbsp;&nbsp;**granite-embedding-reranker-english-r2**  | 149            | 8192        | 55.0      | 44.9      | 54.2        |
| **Retriever: granite-embedding-english-r2**                        | 149            | 8192        | 53.1      | 41.6      | 43.6        |
| &nbsp;&nbsp;&nbsp;&nbsp;ms-marco-MiniLM-L12-v2                     | 33             | 512         | 53.2      | 34.5      | 55.4        |
| &nbsp;&nbsp;&nbsp;&nbsp;bge-reranker-base                          | 278            | 512         | 53.0      | 36.6      | 40.9        |
| &nbsp;&nbsp;&nbsp;&nbsp;bge-reranker-large                         | 560            | 512         | 54.3      | 38.0      | 42.3        |
| &nbsp;&nbsp;&nbsp;&nbsp;gte-reranker-modernbert-base               | 149            | 8192        | 56.1      | 51.2      | 54.8        |
| &nbsp;&nbsp;&nbsp;&nbsp;**granite-embedding-reranker-english-r2**  | 149            | 8192        | 55.8      | 45.8      | 55.2        |


### Model Architecture and Key Features

The latest Granite Reranking r2 release introduces an English ranking model, based on the ModernBERT architecture:
- _granite-embedding-reranker-english-r2_ (**149M** parameters): with an output embedding size of _768_. 

The following table shows the structure of the two R2 models:

| Model                     | granite-embedding-reranker-english-r2   |
| :---------                |:--------:| 
| Embedding size            |  768     | 
| Number of layers          |  22      | 
| Number of attention heads |  12      | 
| Intermediate size         |  1152    | 
| Activation Function       |  GeGLU   | 
| Vocabulary Size           |  50368   | 
| Max. Sequence Length      |  8192    | 
| # Parameters              |  149M    | 


### Training and Optimization

The r2 models incorporate key enhancements from the ModernBERT architecture, including: 
- Alternating attention lengths to accelerate processing 
- Rotary position embeddings for extended sequence length 
- A newly trained tokenizer optimized with code and text data 
- Flash Attention 2.0 for improved efficiency 
- Streamlined parameters, eliminating unnecessary bias terms

## Data Collection
Granite reranking models is trained using data from four key sources: 
1. Unsupervised title-body paired data scraped from the web
2. Publicly available paired with permissive, enterprise-friendly license
3. IBM-internal paired data targetting specific technical domains
4. IBM-generated synthetic data

Notably, we _do not use_ the popular MS-MARCO retrieval dataset in our training corpus due to its non-commercial license (many open-source models use this dataset due to its high quality). 

The underlying encoder models using GneissWeb, an IBM-curated dataset composed exclusively of open, commercial-friendly sources.

For governance, all our data undergoes a data clearance process subject to technical, business, and governance review. 
This comprehensive process captures critical information about the data, including but not limited to their content description ownership, intended use, data classification, licensing information, usage restrictions, how the data will be acquired, as well as an assessment of sensitive information (i.e, personal information). 

## Infrastructure
We train Granite Reranking Model using IBM's computing cluster, BlueVela Cluster, which is outfitted with NVIDIA H100 80gb GPUs. This cluster provides a scalable and efficient infrastructure for training our models over multiple GPUs.

## Ethical Considerations and Limitations
The data used to train the base language model was filtered to remove text containing hate, abuse, and profanity. granite-embedding-reranker-english-r2 is finetuned on English, and has a context length of 8192 tokens (longer texts will be truncated to this size).

## Resources
- ⭐️ Learn about the latest updates with Granite: https://www.ibm.com/granite
- 📄 Get started with tutorials, best practices, and prompt engineering advice: https://www.ibm.com/granite/docs/
- 💡 Learn about the latest Granite learning resources: https://ibm.biz/granite-learning-resources

## Citation
```
@misc{awasthy2025graniteembeddingr2models,
      title={Granite Embedding R2 Models}, 
      author={Parul Awasthy and Aashka Trivedi and Yulong Li and Meet Doshi and Riyaz Bhat and Vignesh P and Vishwajeet Kumar and Yushu Yang and Bhavani Iyer and Abraham Daniels and Rudra Murthy and Ken Barker and Martin Franz and Madison Lee and Todd Ward and Salim Roukos and David Cox and Luis Lastras and Jaydeep Sen and Radu Florian},
      year={2025},
      eprint={2508.21085},
      archivePrefix={arXiv},
      primaryClass={cs.CL},
      url={https://arxiv.org/abs/2508.21085}, 
}
```