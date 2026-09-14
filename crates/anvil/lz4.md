# LZ4-Java Block Format

`sekai-anvil` supports Anvil compression type `4`, which uses Java's
`LZ4BlockOutputStream` framing.

This is not the standard LZ4 frame format. A normal LZ4 frame decoder cannot
be substituted for this format.

## Block layout

Each block has a 21-byte header followed by its body:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | ASCII magic `LZ4Block` |
| 8 | 1 | Token |
| 9 | 4 | Compressed length, little-endian |
| 13 | 4 | Decompressed length, little-endian |
| 17 | 4 | Checksum, little-endian |
| 21 | N | Block body |

The token contains:

- high nibble `0x10`: raw block
- high nibble `0x20`: LZ4-compressed block
- low nibble: compression level

For level `L`, the maximum decompressed block size is:

```text
1 << (10 + L)
````

Only levels `0..=15` are valid.

## Validation

The decoder rejects:

* invalid magic
* unsupported methods
* inconsistent compressed/decompressed lengths
* raw blocks whose compressed and decompressed lengths differ
* blocks exceeding the level-specific size limit
* truncated block bodies
* decompression failures
* output exceeding the global decoder limit
* invalid checksums

A zero-length block terminates the stream and must have a zero checksum.

## Checksum

The checksum uses XXH32 with seed:

```text
0x9747b28c
```

It is calculated over the decompressed block.

The Java implementation drops the upper four bits of the result, so the value
compared against the stored checksum is:

```text
xxh32(data, 0x9747b28c) & 0x0fffffff
```

That masking is part of the compatibility behavior.

## Implementation

The decoder is implemented by `unlz4` in `src/codec.rs`.

Length fields are validated before output allocation, and the global
decompressed-output limit is shared with the Gzip and Zlib paths.

