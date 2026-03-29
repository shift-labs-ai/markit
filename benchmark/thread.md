# Benchmark thread: markit vs Microsoft markitdown

---

1/ we benchmarked markit against microsoft's markitdown on 10 real-world files.

pdf, docx, xlsx, csv, html, epub, pptx — every format that doesn't need an external api.

(image description and audio transcription depend on your llm provider, so those benchmarks would measure the api, not the tool.)

tl;dr: 5-10x faster, same output.

📎 thread_benchmark_speedup.png

---

2/ raw numbers. markit stays under 325ms on everything. markitdown crosses 3 seconds on a 404KB pdf.

the gap is structural — bun vs python startup + runtime. markitdown can't close this.

📎 thread_sidebyside.png

---

3/ "but is the output the same?"

we compared every word both tools extracted. 94-100% identical across all formats. the lowest is pptx at 94.2% — and that's just `<!-- Slide 1 -->` vs `<!-- Slide number: 1 -->`.

📎 thread_similarity.png

---

4/ zooming into the diffs. nearly all green. the tiny slivers are whitespace and markdown syntax choices — `- ` vs `* `, table header formatting, trailing newlines.

no content is lost. no content is different.

📎 thread_diff_breakdown.png

---

5/ the one place markit is actually better: docx tables.

markitdown outputs empty header rows:
```
|  |  |  |  |
| --- | --- | --- | --- |
| Name | Value | Unit |
```

markit gets it right:
```
| Name | Value | Unit |
| --- | --- | --- |
```

---

6/ test corpus:
- bitcoin whitepaper (180KB pdf)
- us constitution (404KB pdf)
- calibre demo (1.3MB docx)
- tech architecture doc (37KB docx)
- microsoft financial sample (81KB xlsx)
- wikipedia markdown page (190KB html)
- alice in wonderland (185KB epub)
- titanic dataset (59KB csv)
- 1000-row customer dataset (87KB csv)
- q4 business review (34KB pptx)

all reproducible: `./benchmark/setup-corpus.sh && ./benchmark/run.sh`

---

7/ markit is a cli and an sdk. pluggable converters, plugin system, llm providers for image description and audio transcription.

5-10x faster than markitdown. same output. better tables.

github.com/Michaelliv/markit

📎 thread_benchmark_speedup.png
