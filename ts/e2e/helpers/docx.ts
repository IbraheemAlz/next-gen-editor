/* Test helper (issue #406): build a minimal `.docx` (one STORED entry per
 * OPC part) around a hand-written `word/document.xml`, so a spec can open a
 * document with exactly the malformed attribute it needs without
 * committing a binary fixture. */
import { appendStoredEntry } from '../zip-append';

const W_NS = 'http://schemas.openxmlformats.org/wordprocessingml/2006/main';

const CONTENT_TYPES =
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">' +
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
    '<Default Extension="xml" ContentType="application/xml"/>' +
    '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>' +
    '</Types>';

const DOT_RELS =
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">' +
    '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>' +
    '</Relationships>';

const DOC_RELS =
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>';

/** An empty zip: only the end-of-central-directory record. */
function emptyZip(): Uint8Array {
    const end = Buffer.alloc(22);
    end.writeUInt32LE(0x06054b50, 0);
    return new Uint8Array(end);
}

/** A `word/document.xml` whose `<w:body>` holds `body` (block content, a
 *  trailing `<w:sectPr>` included when the caller wants one). */
export function documentXml(body: string): string {
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' +
        `<w:document xmlns:w="${W_NS}"><w:body>${body}</w:body></w:document>`
    );
}

/** A minimal OPC package around `document` (a full `word/document.xml`). */
export function storedDocx(document: string): Uint8Array {
    const enc = new TextEncoder();
    let zip = emptyZip();
    for (const [name, xml] of [
        ['[Content_Types].xml', CONTENT_TYPES],
        ['_rels/.rels', DOT_RELS],
        ['word/document.xml', document],
        ['word/_rels/document.xml.rels', DOC_RELS],
    ] as const) {
        zip = appendStoredEntry(zip, name, enc.encode(xml));
    }
    return zip;
}
