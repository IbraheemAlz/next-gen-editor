/* Test helper (issue #567): build a minimal Word 97-2003 binary document
 * (`.doc`, [MS-DOC]) in memory, so a spec can open one through the real File
 * menu without committing a binary fixture. The TypeScript twin of
 * `format_doc::test_builder` for the simple case: ONE uncompressed (UTF-16)
 * piece, one CHPX FKP page, one PAPX FKP page, no section table.
 *
 * WordDocument stream: the FIB at byte 0 (`wIdent` 0xA5EC, `nFib` 0x00C1,
 * `fWhichTblStm` -> 1Table; `csw` 14, `cslw` 22, `cbMac`, `ccpText`;
 * `cbRgFcLcb` 0x5D with the PlcBteChpx (12), PlcBtePapx (13) and Clx (33)
 * pairs), the text from byte 1024 (UTF-16LE, a `\r` mark per paragraph,
 * `\u0007` for a table cell), then the CHPX and PAPX FKP pages (512 bytes,
 * 512-aligned). 1Table stream: the Clx (one Pcdt with one Pcd), then the
 * two bin tables, then (issue #575, when `buildDoc` is given styles) the
 * style sheet (`STSH`, pair 1: `LPStshi` with an 18-byte `Stshif` + `ftcBi`,
 * one `LPStd` per style slot — `StdfBase`, `xstzName`, the `UpxPapx` /
 * `UpxChpx`) and the font table (`SttbfFfn`, pair 15: 1-byte `cchData` +
 * an `FFN` per name). Both streams are padded past 4096 bytes so the
 * compound file needs no mini stream; the container is a version-3 MS-CFB
 * file (512-byte sectors: directory, the two streams, the FAT). */

export interface DocRun {
    text: string;
    bold?: boolean;
    /** `sprmCFBiDi`: a right-to-left run. */
    rtl?: boolean;
    /** `sprmCIstd`: the run's character style (an index into the sheet). */
    charStyle?: number;
}

export interface DocPara {
    runs: DocRun[];
    /** `sprmPFBiDi`: a right-to-left paragraph (`false`: an explicit
     *  left-to-right one, over its style). */
    rtl?: boolean;
    /** `sprmPJc` 1: centered. */
    center?: boolean;
    /** End the paragraph with a table-cell mark (`sprmPFInTable` set). */
    cell?: boolean;
    /** `PapxInFkp.istd`: the paragraph style (default 0, Normal). */
    istd?: number;
}

/** One style of a `DocStyleSheet` (`STD`). */
export interface DocStyle {
    /** `StdfBase.sti`: 0 Normal, 1-9 heading 1-9, 65 Default Paragraph
     *  Font, 0xFFE a user style. */
    sti: number;
    kind: 'paragraph' | 'character';
    /** `istdBase` (default 0xFFF: none). */
    base?: number;
    /** `istdNext` (default 0xFFF: none). */
    next?: number;
    name: string;
    /** The paragraph style's own paragraph sprms (`UpxPapx`). */
    papx?: number[];
    /** The style's own character sprms (`UpxChpx`). */
    chpx?: number[];
}

/** The style sheet and font table `buildDoc` may write. */
export interface DocStyleSheet {
    /** The styles by `istd`; `null` is an empty slot. */
    styles: (DocStyle | null)[];
    /** The font table, by `ftc`. */
    fonts: string[];
    /** `Stshif.ftcAsci`: the default Latin font (default 0). */
    ftcAscii?: number;
}

/** Property modifiers for `DocStyle.papx` / `chpx` ([MS-DOC] 2.6). */
export const docSprm = {
    rtlParagraph: (on: boolean) => [0x41, 0x24, on ? 1 : 0],
    spaceAfter: (twips: number) => [0x14, 0xa4, twips & 0xff, twips >>> 8],
    bold: (on: boolean) => [0x35, 0x08, on ? 1 : 0],
    italic: (on: boolean) => [0x36, 0x08, on ? 1 : 0],
    sizeHalfPoints: (hps: number) => [0x43, 0x4a, hps & 0xff, hps >>> 8],
    /** `sprmCRgFtc0`: the ASCII font, an index into the font table. */
    font: (ftc: number) => [0x4f, 0x4a, ftc & 0xff, ftc >>> 8],
};

const TEXT_AT = 1024;
const SECTOR = 512;
const END_OF_CHAIN = 0xfffffffe;
const FREE = 0xffffffff;
const FAT_SECT = 0xfffffffd;
const NO_STREAM = 0xffffffff;

class Bytes {
    buf: number[] = [];
    get length(): number {
        return this.buf.length;
    }
    u8(v: number): void {
        this.buf.push(v & 0xff);
    }
    u16(v: number): void {
        this.u8(v);
        this.u8(v >>> 8);
    }
    u32(v: number): void {
        this.u16(v & 0xffff);
        this.u16(v >>> 16);
    }
    bytes(b: ArrayLike<number>): void {
        for (let i = 0; i < b.length; i++) this.u8(b[i]!);
    }
    align(to: number): void {
        while (this.buf.length % to !== 0) this.u8(0);
    }
    padTo(n: number): void {
        while (this.buf.length < n) this.u8(0);
    }
    put16(at: number, v: number): void {
        this.buf[at] = v & 0xff;
        this.buf[at + 1] = (v >>> 8) & 0xff;
    }
    put32(at: number, v: number): void {
        this.put16(at, v & 0xffff);
        this.put16(at + 2, v >>> 16);
    }
    out(): Uint8Array {
        return Uint8Array.from(this.buf);
    }
}

/** One FKP page: `entries` are `[fcStart, fcEnd, grpprl]`, contiguous. */
function fkpPage(entries: [number, number, number[]][], papx: boolean): number[] {
    const page = new Array<number>(SECTOR).fill(0);
    const crun = entries.length;
    const perRun = papx ? 13 : 1;
    let top = 511;
    const put32 = (at: number, v: number) => {
        for (let k = 0; k < 4; k++) page[at + k] = (v >>> (8 * k)) & 0xff;
    };
    entries.forEach(([start], i) => put32(4 * i, start));
    put32(4 * crun, entries[crun - 1]![1]);
    entries.forEach(([, , grpprl], i) => {
        let data: number[];
        if (papx) {
            /* PapxInFkp: cb (or 0 + cb'), then the GrpPrlAndIstd (the
             * entry's bytes: istd + grpprl). */
            const len = grpprl.length;
            data = len % 2 === 1 ? [(len + 1) / 2, ...grpprl] : [0, len / 2, ...grpprl];
        } else {
            if (grpprl.length === 0) {
                page[4 * (crun + 1) + i] = 0;
                return;
            }
            data = [grpprl.length, ...grpprl];
        }
        top = (top - data.length) & ~1;
        data.forEach((b, k) => (page[top + k] = b));
        page[4 * (crun + 1) + perRun * i] = top / 2;
    });
    if (top < 4 * (crun + 1) + perRun * crun) throw new Error('the FKP page overflows');
    page[511] = crun;
    return page;
}

/** The `STSH` bytes of `sheet`. */
function styleSheet(sheet: DocStyleSheet): number[] {
    const v = new Bytes();
    const ftc = sheet.ftcAscii ?? 0;
    v.u16(20); // cbStshi: Stshif (18) + ftcBi
    v.u16(sheet.styles.length); // cstd
    v.u16(10); // cbSTDBaseInFile: StdfBase only
    v.u16(1); // fStdStylenamesWritten
    v.u16(0x5b); // stiMaxWhenSaved
    v.u16(0x0f); // istdMaxFixedWhenSaved
    v.u16(0); // nVerBuiltInNamesWhenSaved
    v.u16(ftc); // ftcAsci
    v.u16(ftc); // ftcFE
    v.u16(ftc); // ftcOther
    v.u16(ftc); // ftcBi
    for (const s of sheet.styles) {
        if (!s) {
            v.u16(0);
            continue;
        }
        const std = new Bytes();
        const paragraph = s.kind === 'paragraph';
        std.u16(s.sti & 0xfff);
        std.u16((paragraph ? 1 : 2) | ((s.base ?? 0xfff) << 4));
        std.u16((paragraph ? 2 : 1) | ((s.next ?? 0xfff) << 4)); // cupx | istdNext
        std.u16(0); // bchUpe (patched below)
        std.u16(0); // grfstd
        std.u16(s.name.length);
        for (let i = 0; i < s.name.length; i++) std.u16(s.name.charCodeAt(i));
        std.u16(0);
        const upx = (data: number[]) => {
            std.u16(data.length);
            std.bytes(data);
            if (data.length % 2 === 1) std.u8(0);
        };
        if (paragraph) upx([0, 0, ...(s.papx ?? [])]); // UpxPapx: istd + grpprl
        upx(s.chpx ?? []);
        std.put16(6, std.length);
        v.u16(std.length);
        v.bytes(std.out());
    }
    return v.buf;
}

/** The `SttbfFfn` bytes of `fonts`. */
function fontTable(fonts: string[]): number[] {
    const v = new Bytes();
    v.u16(fonts.length); // cData
    v.u16(0); // cbExtra
    for (const name of fonts) {
        const ffn = new Bytes();
        ffn.u8(0x26); // prq 2, fTrueType, ff 2
        ffn.u16(400); // wWeight
        ffn.padTo(39); // chs, ixchSzAlt, panose, fs
        for (let i = 0; i < name.length; i++) ffn.u16(name.charCodeAt(i));
        ffn.u16(0);
        v.u8(ffn.length);
        v.bytes(ffn.out());
    }
    return v.buf;
}

/** The two streams of a document made of `paras`. */
function streams(
    paras: DocPara[],
    sheet?: DocStyleSheet,
): { word: Uint8Array; table: Uint8Array } {
    const word = new Bytes();
    word.padTo(TEXT_AT);
    const chpx: [number, number, number[]][] = [];
    const papx: [number, number, number[]][] = [];
    let cps = 0;
    for (const p of paras) {
        const paraStart = word.length;
        for (const r of p.runs) {
            const start = word.length;
            for (let i = 0; i < r.text.length; i++) word.u16(r.text.charCodeAt(i));
            cps += r.text.length;
            const g: number[] = [];
            if (r.bold) g.push(0x35, 0x08, 0x01); // sprmCFBold
            if (r.rtl) g.push(0x5a, 0x08, 0x01); // sprmCFBiDi
            if (r.charStyle !== undefined) {
                g.push(0x30, 0x4a, r.charStyle & 0xff, r.charStyle >>> 8); // sprmCIstd
            }
            if (word.length > start) chpx.push([start, word.length, g]);
        }
        const markAt = word.length;
        word.u16(p.cell ? 0x0007 : 0x000d);
        cps += 1;
        chpx.push([markAt, word.length, []]);
        const istd = p.istd ?? 0;
        const g: number[] = [istd & 0xff, istd >>> 8];
        if (p.rtl !== undefined) g.push(0x41, 0x24, p.rtl ? 1 : 0); // sprmPFBiDi
        if (p.center) g.push(0x61, 0x24, 0x01); // sprmPJc
        if (p.cell) g.push(0x16, 0x24, 0x01); // sprmPFInTable
        papx.push([paraStart, word.length, g]);
    }
    const textEnd = word.length;

    word.align(SECTOR);
    const chpxPn = word.length / SECTOR;
    word.bytes(fkpPage(chpx, false));
    const papxPn = word.length / SECTOR;
    word.bytes(fkpPage(papx, true));

    const table = new Bytes();
    /* Clx: one Pcdt (clxt 2, lcb) holding PlcPcd = aCp[2] + one Pcd. */
    table.u8(0x02);
    table.u32(4 * 2 + 8);
    table.u32(0);
    table.u32(cps);
    table.u16(0); // Pcd flags
    table.u32(TEXT_AT); // FcCompressed: fCompressed 0, fc = byte offset
    table.u16(0); // Prm
    const clx = [0, table.length];
    const bin = (pn: number) => {
        table.align(2);
        const at = table.length;
        table.u32(TEXT_AT);
        table.u32(textEnd);
        table.u32(pn);
        return [at, table.length - at];
    };
    const bteChpx = bin(chpxPn);
    const btePapx = bin(papxPn);
    const place = (bytes: number[]) => {
        table.align(2);
        const at = table.length;
        table.bytes(bytes);
        return [at, bytes.length];
    };
    const stsh = sheet ? place(styleSheet(sheet)) : [0, 0];
    const ffn = sheet ? place(fontTable(sheet.fonts)) : [0, 0];

    /* The FIB. */
    word.put16(0x00, 0xa5ec); // wIdent
    word.put16(0x02, 0x00c1); // nFib
    word.put16(0x06, 0x0409); // lid
    word.put16(0x0a, 0x0200); // fWhichTblStm: 1Table
    word.put16(0x0c, 0x00bf); // nFibBack
    word.put16(0x20, 0x000e); // csw
    word.put16(0x3e, 0x0016); // cslw
    word.padTo(4096);
    table.padTo(4096);
    word.put32(0x40, word.length); // cbMac
    word.put32(0x40 + 3 * 4, cps); // ccpText
    word.put16(0x98, 0x5d); // cbRgFcLcb
    const pair = (i: number, [fc, lcb]: number[]) => {
        word.put32(0x9a + 8 * i, fc!);
        word.put32(0x9a + 8 * i + 4, lcb!);
    };
    pair(1, stsh);
    pair(12, bteChpx);
    pair(13, btePapx);
    pair(15, ffn);
    pair(33, clx);
    return { word: word.out(), table: table.out() };
}

/** A version-3 compound file whose root storage holds `entries` (each
 *  at least 4096 bytes, so every stream lives in regular sectors). */
function compoundFile(entries: [string, Uint8Array][]): Uint8Array {
    const sectors: Uint8Array[] = [];
    const fat: number[] = [];
    const chain = (data: Uint8Array): number => {
        const start = sectors.length;
        const n = Math.max(1, Math.ceil(data.length / SECTOR));
        for (let k = 0; k < n; k++) {
            const sec = new Uint8Array(SECTOR);
            sec.set(data.subarray(k * SECTOR, Math.min((k + 1) * SECTOR, data.length)));
            sectors.push(sec);
            fat.push(k + 1 === n ? END_OF_CHAIN : start + k + 1);
        }
        return start;
    };
    /* The directory sector is filled in once the streams are placed. */
    chain(new Uint8Array(SECTOR));
    const starts = entries.map(([, data]) => chain(data));
    let fatSecs = 1;
    while ((sectors.length + fatSecs) * 4 > fatSecs * SECTOR) fatSecs += 1;
    const fatStart = sectors.length;
    for (let k = 0; k < fatSecs; k++) fat.push(FAT_SECT);
    while (fat.length < fatSecs * (SECTOR / 4)) fat.push(FREE);

    /* Directory: root (child = the first stream), the streams chained by
     * their right sibling. */
    const dir = new Bytes();
    const entry = (
        name: string,
        type: number,
        right: number,
        child: number,
        start: number,
        size: number,
    ) => {
        const at = dir.length;
        for (let i = 0; i < name.length; i++) dir.u16(name.charCodeAt(i));
        dir.padTo(at + 64);
        dir.u16(name.length ? 2 * (name.length + 1) : 0);
        dir.u8(type);
        dir.u8(1); // black
        dir.u32(NO_STREAM); // left
        dir.u32(right);
        dir.u32(child);
        dir.padTo(at + 0x74);
        dir.u32(start);
        dir.u32(size);
        dir.u32(0);
    };
    entry('Root Entry', 5, NO_STREAM, 1, END_OF_CHAIN, 0);
    entries.forEach(([name, data], i) => {
        entry(name, 2, i + 1 < entries.length ? i + 2 : NO_STREAM, NO_STREAM, starts[i]!, data.length);
    });
    while (dir.length < SECTOR) entry('', 0, NO_STREAM, NO_STREAM, 0, 0);
    sectors[0] = dir.out();

    const header = new Bytes();
    header.bytes([0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]);
    header.padTo(0x18);
    header.u16(0x003e); // minor version
    header.u16(0x0003); // major version
    header.u16(0xfffe); // byte order
    header.u16(9); // sector shift
    header.u16(6); // mini sector shift
    header.padTo(0x2c);
    header.u32(fatSecs);
    header.u32(0); // first directory sector
    header.u32(0); // transaction signature
    header.u32(4096); // mini stream cutoff
    header.u32(END_OF_CHAIN); // first mini FAT sector
    header.u32(0);
    header.u32(END_OF_CHAIN); // first DIFAT sector
    header.u32(0);
    for (let i = 0; i < 109; i++) header.u32(i < fatSecs ? fatStart + i : FREE);

    const fatBytes = new Bytes();
    for (const v of fat) fatBytes.u32(v);
    const fatData = fatBytes.out();
    for (let k = 0; k < fatSecs; k++) {
        sectors.push(fatData.subarray(k * SECTOR, (k + 1) * SECTOR));
    }

    const out = new Uint8Array(SECTOR * (1 + sectors.length));
    out.set(header.out(), 0);
    sectors.forEach((s, i) => out.set(s, SECTOR * (i + 1)));
    return out;
}

/** The `.doc` file of `paras`, with `sheet`'s style sheet and font table
 *  when given. */
export function buildDoc(paras: DocPara[], sheet?: DocStyleSheet): Uint8Array {
    const { word, table } = streams(paras, sheet);
    return compoundFile([
        ['WordDocument', word],
        ['1Table', table],
    ]);
}
