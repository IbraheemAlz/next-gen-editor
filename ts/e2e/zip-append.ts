/* Test helper (issue #212): append one STORED entry to an existing .zip
 * (.docx) — enough to give a fixture a large non-media part, like the
 * embedded font (`word/fonts/font1.odttf`) a real Word document carries,
 * without committing a multi-megabyte binary. Plain OPC zip only: no
 * zip64, no archive comment handling beyond locating the end record. */
import { crc32 } from 'node:zlib';

const EOCD_SIG = 0x06054b50;
const LOCAL_SIG = 0x04034b50;
const CENTRAL_SIG = 0x02014b50;
/** MS-DOS date 1980-01-01 — a fixed stamp keeps the output deterministic. */
const DOS_DATE = 0x21;

export function appendStoredEntry(zip: Uint8Array, name: string, data: Uint8Array): Uint8Array {
    const src = Buffer.from(zip.buffer, zip.byteOffset, zip.byteLength);
    let eocd = -1;
    for (let i = src.length - 22; i >= 0; i--) {
        if (src.readUInt32LE(i) === EOCD_SIG) {
            eocd = i;
            break;
        }
    }
    if (eocd < 0) throw new Error('appendStoredEntry: no end-of-central-directory record');
    const entries = src.readUInt16LE(eocd + 10);
    const cdSize = src.readUInt32LE(eocd + 12);
    const cdOffset = src.readUInt32LE(eocd + 16);
    const nameBytes = Buffer.from(name, 'utf8');
    const crc = crc32(data);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(LOCAL_SIG, 0);
    local.writeUInt16LE(20, 4); // version needed
    local.writeUInt16LE(0, 6); // flags
    local.writeUInt16LE(0, 8); // method: stored
    local.writeUInt16LE(0, 10); // time
    local.writeUInt16LE(DOS_DATE, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    local.writeUInt16LE(0, 28); // extra length

    const central = Buffer.alloc(46);
    central.writeUInt32LE(CENTRAL_SIG, 0);
    central.writeUInt16LE(20, 4); // version made by
    central.writeUInt16LE(20, 6); // version needed
    central.writeUInt16LE(0, 8);
    central.writeUInt16LE(0, 10);
    central.writeUInt16LE(0, 12);
    central.writeUInt16LE(DOS_DATE, 14);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    // extra, comment, disk, internal/external attrs: zero
    central.writeUInt32LE(cdOffset, 42); // local header offset

    const newCdOffset = cdOffset + local.length + nameBytes.length + data.length;
    const newCdSize = cdSize + central.length + nameBytes.length;
    const end = Buffer.alloc(22);
    end.writeUInt32LE(EOCD_SIG, 0);
    end.writeUInt16LE(entries + 1, 8);
    end.writeUInt16LE(entries + 1, 10);
    end.writeUInt32LE(newCdSize, 12);
    end.writeUInt32LE(newCdOffset, 16);

    return new Uint8Array(
        Buffer.concat([
            src.subarray(0, cdOffset),
            local,
            nameBytes,
            Buffer.from(data.buffer, data.byteOffset, data.byteLength),
            src.subarray(cdOffset, cdOffset + cdSize),
            central,
            nameBytes,
            end,
        ]),
    );
}

/** Deterministic incompressible bytes (xorshift32) — font-like payload. */
export function pseudoRandomBytes(len: number, seed = 0x2127_2120): Uint8Array {
    const out = new Uint8Array(len);
    let x = seed >>> 0 || 1;
    for (let i = 0; i < len; i++) {
        x ^= x << 13;
        x >>>= 0;
        x ^= x >>> 17;
        x ^= x << 5;
        x >>>= 0;
        out[i] = x & 0xff;
    }
    return out;
}
