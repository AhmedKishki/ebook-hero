"""Label the sampled pages of each book with docling's layout model.

usage: python3 docling_label.py books.tsv BOOK_DIR OUT_DIR

Run it with a Python that has docling installed (2.135.0 was used). Writes
OUT_DIR/<sha256>.json: {page: [{label, bbox: [left, top, right, bottom], text}]},
boxes in top-origin points of the page's CropBox. Skips books already labelled.
OCR and table structure are off, as the evaluation compares text-layer PDFs.
Needs mutool (MuPDF) to cut the sampled pages out of each book.
"""
import json, os, subprocess, sys, tempfile

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from docling_core.types.doc import ContentLayer

books, book_dir, out_dir = sys.argv[1], sys.argv[2], sys.argv[3]
options = PdfPipelineOptions()
options.do_ocr = False
options.do_table_structure = False
options.generate_page_images = False
options.accelerator_options.device = "cpu"
options.accelerator_options.num_threads = 8
converter = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=options)})

os.makedirs(out_dir, exist_ok=True)
for line in list(open(books))[1:]:
    sha, _, sample, name = line.rstrip("\n").split("\t")
    out = os.path.join(out_dir, f"{sha}.json")
    if os.path.exists(out):
        continue
    pages = [int(p) for p in sample.split(",")]
    with tempfile.TemporaryDirectory() as tmp:
        cut = os.path.join(tmp, "pages.pdf")
        subprocess.run(["mutool", "merge", "-o", cut, os.path.join(book_dir, name), ",".join(map(str, pages))],
                       check=True, capture_output=True)
        doc = converter.convert(cut).document
    labels = {p: [] for p in pages}
    for item, _ in doc.iterate_items(included_content_layers=set(ContentLayer)):
        for prov in getattr(item, "prov", []):
            page = pages[prov.page_no - 1]
            box = prov.bbox.to_top_left_origin(page_height=doc.pages[prov.page_no].size.height)
            labels[page].append({
                "label": str(getattr(item.label, "value", item.label)),
                "bbox": [round(box.l, 1), round(box.t, 1), round(box.r, 1), round(box.b, 1)],
                "text": getattr(item, "text", "") or "",
            })
    with open(out, "w") as f:
        json.dump(labels, f)
    print(sha[:10], len(pages), "pages", flush=True)
