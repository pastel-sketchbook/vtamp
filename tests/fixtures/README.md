# Generated audio fixture

`extended-mdat.m4a` contains a 0.2-second, 440 Hz sine wave generated for vtamp's tests. It contains no third-party music or artwork and is distributed under the project's MIT license.

It was generated with FFmpeg's `sine` source and AAC encoder. The adjacent eight-byte `free` atom and normal `mdat` header were replaced with a sixteen-byte, extended-size `mdat` header. File length and audio offsets are unchanged. This reproduces the MP4 layout in the local development samples without redistributing those recordings.

The regression test verifies both metadata extraction and actual AAC sample decoding. FFmpeg is not needed to run the tests or vtamp.
