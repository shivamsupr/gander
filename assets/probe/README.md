# Probe files

`gander --check --deep` runs these three files through your backends. They are
built into the binary.

| File | Content | The answer must contain |
|---|---|---|
| `probe.png` | the text `GANDER PROBE 4172` on a blue background | the number `4172` |
| `probe.m4a` | a synthetic voice, 3.8 seconds | most words of the sentence below |
| `probe.mp4` | the image and the audio together | both |

The spoken sentence is: "This is the gander audio probe. The code word is copper
seven four."

## How to build them again

The files come from ffmpeg alone. The voice is `slt` from libflite, which is free
to redistribute. Your ffmpeg needs the `drawtext` and `flite` filters.

```sh
TXT="This is the gander audio probe. The code word is copper seven four."

ffmpeg -y -f lavfi -i "color=c=0x1e3a5f:s=480x270:d=1" \
  -vf "drawtext=text='GANDER PROBE 4172':fontcolor=white:fontsize=40:x=(w-text_w)/2:y=(h-text_h)/2" \
  -frames:v 1 probe.png

ffmpeg -y -f lavfi -i "flite=text='$TXT':voice=slt" \
  -ac 1 -ar 16000 -c:a aac -b:a 24k probe.m4a

D=$(ffprobe -v error -show_entries format=duration -of csv=p=0 probe.m4a)
ffmpeg -y -loop 1 -i probe.png -i probe.m4a -c:v libx264 -tune stillimage \
  -pix_fmt yuv420p -r 5 -crf 32 -c:a copy -t "$D" -movflags +faststart probe.mp4
```

If you change the text or the sentence, change `SEEN_MARK` and `SPOKEN` in
`src/deep.rs` too.
