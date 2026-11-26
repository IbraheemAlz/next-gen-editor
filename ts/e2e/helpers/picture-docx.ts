/* Shared `.docx` fixtures with pictures, built in the test (no binary in the
 * tree): solid-colour PNGs, a minimal STORED package with an optional header
 * part, inline picture runs. First written for epic #555 row 11
 * (`text-cursor-matrix.spec.ts`); shared since issues #573 / #574. */
import { crc32, deflateSync } from 'node:zlib';
import { appendStoredEntry } from '../zip-append';

/** A solid `w` x `h` RGB PNG. */
export function png(w: number, h: number, rgb: [number, number, number]): Uint8Array {
    const chunk = (type: string, data: Buffer): Buffer => {
        const len = Buffer.alloc(4);
        len.writeUInt32BE(data.length);
        const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
        const crc = Buffer.alloc(4);
        crc.writeUInt32BE(crc32(td) >>> 0);
        return Buffer.concat([len, td, crc]);
    };
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(w, 0);
    ihdr.writeUInt32BE(h, 4);
    ihdr[8] = 8; // bit depth
    ihdr[9] = 2; // RGB
    const raw = Buffer.alloc((w * 3 + 1) * h);
    for (let y = 0; y < h; y++) {
        for (let x = 0; x < w; x++) raw.set(rgb, y * (w * 3 + 1) + 1 + x * 3);
    }
    return new Uint8Array(
        Buffer.concat([
            Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
            chunk('IHDR', ihdr),
            chunk('IDAT', deflateSync(raw)),
            chunk('IEND', Buffer.alloc(0)),
        ]),
    );
}

export const NS =
    'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" ' +
    'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" ' +
    'xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" ' +
    'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" ' +
    'xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"';
export const REL = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships/';
export const XML = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>';
/** A4, 1 in side / bottom margins; `top` (twips) and the header distance
 *  1/2 in. */
export const sectPr = (top: number, refs = ''): string =>
    `<w:sectPr>${refs}<w:pgSz w:w="11906" w:h="16838"/>` +
    `<w:pgMar w:top="${top}" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/>` +
    '</w:sectPr>';

export type Rel = [id: string, type: string, target: string];

/** A minimal STORED `.docx`: `body` (already holding its `<w:sectPr>`),
 *  an optional header part, and media parts. */
export function docxPackage(p: {
    body: string;
    docRels?: Rel[];
    header?: { xml: string; rels: Rel[] };
    media?: Array<[string, Uint8Array]>;
}): Uint8Array {
    const rels = (rows: Rel[]): string =>
        `${XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">` +
        rows.map(([id, type, target]) => `<Relationship Id="${id}" Type="${type}" Target="${target}"/>`).join('') +
        '</Relationships>';
    const text = (s: string): Uint8Array => new TextEncoder().encode(s);
    const entries: Array<[string, Uint8Array]> = [
        [
            '[Content_Types].xml',
            text(
                `${XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">` +
                    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
                    '<Default Extension="xml" ContentType="application/xml"/>' +
                    '<Default Extension="png" ContentType="image/png"/>' +
                    '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>' +
                    (p.header
                        ? '<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>'
                        : '') +
                    '</Types>',
            ),
        ],
        ['_rels/.rels', text(rels([['rId1', `${REL}officeDocument`, 'word/document.xml']]))],
        ['word/document.xml', text(`${XML}<w:document ${NS}><w:body>${p.body}</w:body></w:document>`)],
        ['word/_rels/document.xml.rels', text(rels(p.docRels ?? []))],
    ];
    if (p.header) {
        entries.push(['word/header1.xml', text(`${XML}<w:hdr ${NS}>${p.header.xml}</w:hdr>`)]);
        entries.push(['word/_rels/header1.xml.rels', text(rels(p.header.rels))]);
    }
    for (const m of p.media ?? []) entries.push(m);
    const eocd = new Uint8Array(22);
    new DataView(eocd.buffer).setUint32(0, 0x06054b50, true);
    let zip = eocd;
    for (const [name, data] of entries) zip = appendStoredEntry(zip, name, data);
    return zip;
}

/** One inline picture run, 1 in x 1/2 in, embedding `rid`. */
export const pictureRun = (rid: string): string =>
    '<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0">' +
    '<wp:extent cx="914400" cy="457200"/><wp:docPr id="1" name="Picture"/>' +
    '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>' +
    '<pic:nvPicPr><pic:cNvPr id="1" name="p"/><pic:cNvPicPr/></pic:nvPicPr>' +
    `<pic:blipFill><a:blip r:embed="${rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>` +
    '<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm>' +
    '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>' +
    '</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>';

/** A page with a picture in its header and another in the body. Margins:
 *  top 2 in, left 1 in, header distance 1/2 in — so the header picture
 *  sits at x 72..144 pt, y 36..72 pt (+ its line's descent). */
export function pictureDocx(): Uint8Array {
    return docxPackage({
        body:
            '<w:p><w:r><w:t xml:space="preserve">Body text above the picture</w:t></w:r></w:p>' +
            `<w:p>${pictureRun('rId5')}</w:p>` +
            '<w:p><w:r><w:t xml:space="preserve">Body text below</w:t></w:r></w:p>' +
            sectPr(2880, '<w:headerReference w:type="default" r:id="rId7"/>'),
        docRels: [
            ['rId5', `${REL}image`, 'media/image1.png'],
            ['rId7', `${REL}header`, 'header1.xml'],
        ],
        header: {
            xml: `<w:p>${pictureRun('rId5')}</w:p>`,
            rels: [['rId5', `${REL}image`, 'media/image2.png']],
        },
        media: [
            ['word/media/image1.png', png(64, 32, [32, 176, 64])],
            ['word/media/image2.png', png(64, 32, [208, 32, 32])],
        ],
    });
}

/** Issue #573 — one FLOATING picture run (`<wp:anchor>`, no text wrap),
 *  `cx` x `cy` EMU, its top-left `x` / `y` EMU from the page's top-left
 *  corner, embedding `rid`. */
export const floatingPictureRun = (
    rid: string,
    p: { x: number; y: number; cx: number; cy: number; id: number },
): string =>
    '<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" ' +
    `relativeHeight="${p.id}" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">` +
    '<wp:simplePos x="0" y="0"/>' +
    `<wp:positionH relativeFrom="page"><wp:posOffset>${p.x}</wp:posOffset></wp:positionH>` +
    `<wp:positionV relativeFrom="page"><wp:posOffset>${p.y}</wp:posOffset></wp:positionV>` +
    `<wp:extent cx="${p.cx}" cy="${p.cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/>` +
    `<wp:docPr id="${p.id}" name="Float ${p.id}"/><wp:cNvGraphicFramePr/>` +
    '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>' +
    `<pic:nvPicPr><pic:cNvPr id="${p.id}" name="f"/><pic:cNvPicPr/></pic:nvPicPr>` +
    `<pic:blipFill><a:blip r:embed="${rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>` +
    `<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="${p.cx}" cy="${p.cy}"/></a:xfrm>` +
    '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>' +
    '</pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>';
