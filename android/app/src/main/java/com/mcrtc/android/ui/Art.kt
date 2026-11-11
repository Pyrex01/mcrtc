// Procedural textures: seeded noise, drawn here, so the app ships no artwork. A port of
// `src/ui/art.rs` with the same seeds, so both frontends draw the same pixels.

package com.mcrtc.android.ui

import android.graphics.Bitmap
import android.graphics.Color

/** Deterministic, so every run and every device draws the same pixels. */
private class Rng(private var state: Int) {
    fun next(): Double {
        state = state * 1664525 + 1013904223
        return (state.toLong() and 0xffffffffL).toDouble() / 4294967296.0
    }

    fun below(limit: Int): Int = (next() * limit).toInt()
}

/** Saturation and lightness run from 0 to 1, the hue from 0 to 360. */
private fun hsl(hue: Float, saturation: Float, lightness: Float): Int {
    val chroma = (1f - kotlin.math.abs(2f * lightness - 1f)) * saturation
    fun channel(offset: Float): Int {
        val k = (offset + hue / 30f) % 12f
        val value = lightness - chroma / 2f * minOf(k - 3f, 9f - k).coerceIn(-1f, 1f)
        return Math.round(value * 255f)
    }
    return Color.rgb(channel(0f), channel(8f), channel(4f))
}

private class Canvas(val width: Int, val height: Int, fill: Int) {
    val pixels = IntArray(width * height) { fill }

    /** Paints `color` over a rectangle, blending by its alpha. */
    fun fill(x: Int, y: Int, width: Int, height: Int, color: Int) {
        val alpha = Color.alpha(color)
        for (row in y until y + height) {
            for (column in x until x + width) {
                val index = row * this.width + column
                val under = pixels[index]
                fun mix(top: Int, bottom: Int) = (top * alpha + bottom * (255 - alpha)) / 255
                pixels[index] = Color.rgb(
                    mix(Color.red(color), Color.red(under)),
                    mix(Color.green(color), Color.green(under)),
                    mix(Color.blue(color), Color.blue(under)),
                )
            }
        }
    }

    fun bitmap(): Bitmap = Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888)
}

private fun noiseTile(palette: IntArray, seed: Int): Bitmap {
    val random = Rng(seed)
    val canvas = Canvas(16, 16, Color.BLACK)
    for (index in canvas.pixels.indices) {
        canvas.pixels[index] = palette[random.below(palette.size)] or 0xff000000.toInt()
    }
    return canvas.bitmap()
}

object Art {
    val earth: Bitmap by lazy {
        noiseTile(intArrayOf(0x2e2118, 0x2a1d15, 0x352619, 0x241912, 0x3a2a1c, 0x2e2118, 0x1f1610), 7)
    }

    val stone: Bitmap by lazy {
        noiseTile(intArrayOf(0x6c6c6c, 0x666666, 0x616161, 0x707070, 0x5d5d5d, 0x686868), 21)
    }

    /** The wordmark: an original block font, one bevelled stone cube per glyph pixel. */
    val logo: Bitmap by lazy { drawLogo() }

    private val glyphM = listOf("11000011", "11100111", "11111111", "11011011", "11000011", "11000011", "11000011")
    private val glyphC = listOf("0111111", "1111111", "1100000", "1100000", "1100000", "1111111", "0111111")
    private val glyphR = listOf("1111110", "1111111", "1100011", "1111110", "1101100", "1100110", "1100011")
    private val glyphT = listOf("11111111", "11111111", "00011000", "00011000", "00011000", "00011000", "00011000")

    private fun drawLogo(): Bitmap {
        val cell = 5
        val depth = 3
        val spacing = 1

        val cells = mutableListOf<Pair<Int, Int>>()
        var offset = 0
        for (glyph in listOf(glyphM, glyphC, glyphR, glyphT, glyphC)) {
            glyph.forEachIndexed { y, line ->
                line.forEachIndexed { x, pixel ->
                    if (pixel == '1') cells += (offset + x) * cell to y * cell
                }
            }
            offset += glyph[0].length + spacing
        }
        val columns = offset - spacing

        val canvas = Canvas(columns * cell + depth, 7 * cell + depth, Color.TRANSPARENT)
        // The shadow goes down first, so it can never cover a neighbouring block.
        for ((left, top) in cells) {
            for (row in top + depth until top + depth + cell) {
                for (column in left + depth until left + depth + cell) {
                    canvas.pixels[row * canvas.width + column] = Color.BLACK
                }
            }
        }
        val random = Rng(99)
        for ((left, top) in cells) {
            for (row in top until top + cell) {
                for (column in left until left + cell) {
                    val shade = (176 - top * 2 / 3 + random.below(34)).coerceIn(0, 255)
                    canvas.pixels[row * canvas.width + column] = Color.rgb(shade, shade, shade)
                }
            }
            canvas.fill(left, top, cell, 1, Color.argb((0.35 * 255).toInt(), 255, 255, 255))
            canvas.fill(left, top + cell - 1, cell, 1, Color.argb((0.3 * 255).toInt(), 0, 0, 0))
        }
        return canvas.bitmap()
    }

    private val portraits = HashMap<String, Bitmap>()

    /** A small pixel portrait derived from the name, so friends are easy to tell apart. */
    fun portrait(name: String): Bitmap = portraits.getOrPut(name) {
        var seed = 2166136261L.toInt()
        for (letter in name) seed = (seed xor letter.code) * 16777619
        val random = Rng(seed)
        val hue = random.below(360).toFloat()
        val skin = intArrayOf(0xc69c6d, 0xe0b48a, 0x8d5a3a, 0xf0c8a0)[random.below(4)] or 0xff000000.toInt()

        val canvas = Canvas(8, 8, skin)
        val hair = hsl(hue, 0.45f, 0.28f)
        canvas.fill(0, 0, 8, 2, hair)
        for (x in 0 until 8) {
            if (random.next() > 0.45) canvas.fill(x, 2, 1, 1, hair)
        }
        canvas.fill(0, 2, 1, 3, hair)
        canvas.fill(7, 2, 1, 3, hair)
        canvas.fill(1, 4, 2, 1, Color.WHITE)
        canvas.fill(5, 4, 2, 1, Color.WHITE)
        val eyes = hsl((hue + 150f) % 360f, 0.6f, 0.35f)
        canvas.fill(2, 4, 1, 1, eyes)
        canvas.fill(5, 4, 1, 1, eyes)
        canvas.fill(3, 6, 2, 1, Color.argb((0.45 * 255).toInt(), 0, 0, 0))
        canvas.bitmap()
    }
}
