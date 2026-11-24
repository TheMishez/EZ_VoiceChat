**Lighter in-game overlay.** The "who's talking" overlay is now drawn as a plain Windows window instead of a hidden browser page, so it uses a small fraction of the memory and GPU while you play. It only redraws when something changes (someone starts or stops talking, mutes, joins).

Also: the audio thread no longer allocates memory while you talk, which keeps voice timing steady on busy PCs.

If you're on 0.1.2, this should arrive through **Restart to update** on its own.
