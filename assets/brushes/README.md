# Paper grain attribution

`revoy-paper.gray` is the 512 × 512 grayscale form of the embedded
`10_drawed_dotted.png` texture from **Krita brushes: Charcoal pencils** by
**David Revoy, https://www.davidrevoy.com**, licensed **CC BY 4.0**:
https://creativecommons.org/licenses/by/4.0/

Source: https://www.davidrevoy.com/article326/krita-brushes-charcoal-pencils
Download: https://www.peppercarrot.com/extras/resources/2017-01-18_Charcoal_pencils.zip
Embedded in `deevad 1d1 charcoal pencil thin.kpp` in `Charcoal_pencils.bundle`.
Original embedded PNG SHA-256:
`0da7511babfea51ae41e44fba852de4b99aa7caaa56f56f96467f1d6b655c000`.

Modification: decoded indexed PNG to row-major grayscale bytes (one byte per
pixel, no resizing). Sketchpad's contact geometry, deposit equations and GPU
implementation are our own; this is not a port of Krita's brush engine.
