// The animated parts: the title screen's logo and splash, and the loading dots.

package com.mcrtc.android.ui

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.os.SystemClock
import android.view.View
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.sin

private val SPLASHES = listOf(
    "No port forwarding!",
    "Copy, paste, play!",
    "Peer to peer!",
    "Also try TURN!",
    "Now with 0 VPNs!",
    "127.0.0.1 is home!",
    "Bring a friend!",
    "NAT? What NAT?",
    "16 KiB at a time!",
    "Reliable and ordered!",
    "Pocket sized!",
)

private val LOADING = listOf("O o o", "o O o", "o o O", "o O o")

fun randomSplash(): String = SPLASHES.random()

/** The logo with its splash, and the version and legal lines along the bottom. */
@SuppressLint("ViewConstructor")
class TitleArt(
    context: Context,
    private val gui: Gui,
    private val splash: String,
    private val version: String,
) : View(context) {
    private val paint = gui.paint()
    private val splashPaint = gui.paint(YELLOW)
    private val pixels = Paint().apply { isFilterBitmap = false }

    init {
        contentDescription = "mcrtc"
    }

    override fun onDraw(canvas: Canvas) {
        val unit = gui.scale
        val logo = Art.logo
        val left = width / 2 - logo.width * unit / 2
        val top = 30 * unit
        val area = Rect(left, top, left + logo.width * unit, top + logo.height * unit)
        canvas.drawBitmap(logo, null, area, pixels)

        val seconds = SystemClock.uptimeMillis() % 1000 / 1000.0
        val pulse = 1.06f - abs(sin(seconds * 2 * PI)).toFloat() * 0.12f
        val centerX = (area.right - 28 * unit).toFloat()
        val centerY = (area.bottom - 2 * unit).toFloat()
        canvas.save()
        canvas.rotate(-20f, centerX, centerY)
        canvas.scale(pulse, pulse, centerX, centerY)
        val capTop = Math.round(centerY - gui.capHeight(splashPaint) / 2f)
        gui.text(canvas, splash, centerX, capTop, splashPaint, YELLOW, Paint.Align.CENTER)
        canvas.restore()

        val bottom = height - 10 * unit
        gui.text(canvas, version, (2 * unit).toFloat(), bottom, paint, WHITE, Paint.Align.LEFT)
        val legal = "Not affiliated with Mojang or Microsoft"
        gui.text(canvas, legal, (width - 2 * unit).toFloat(), bottom, paint, WHITE, Paint.Align.RIGHT)
        // About 30 frames a second, like the desktop window.
        postInvalidateDelayed(33)
    }
}

/** The bouncing dots of a screen that waits. */
fun dots(context: Context, gui: Gui): McText {
    val view = McText(context, gui, LOADING[0], MUTED, center = true)
    // Pure decoration; announcing every frame would drown out a screen reader.
    view.importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
    val tick = object : Runnable {
        override fun run() {
            view.text = LOADING[(SystemClock.uptimeMillis() / 300 % LOADING.size).toInt()]
            view.postDelayed(this, 100)
        }
    }
    view.addOnAttachStateChangeListener(object : View.OnAttachStateChangeListener {
        override fun onViewAttachedToWindow(view: View) {
            view.post(tick)
        }

        override fun onViewDetachedFromWindow(view: View) {
            view.removeCallbacks(tick)
        }
    })
    return view
}
