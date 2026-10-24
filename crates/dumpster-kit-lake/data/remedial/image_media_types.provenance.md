# image_media_types.csv

The image media types a binary lake part (`PartFormat::Bin`) can declare, with the file extension
its object key carries.

Source: IANA media types registry (image/png, image/jpeg, image/webp, image/gif, image/avif,
image/svg+xml); extensions are the conventional ones, with `jpg` for JPEG. Read by
`dumpster-kit-lake` (`keys.rs`) to name a binary part's key and to recover its media type.
