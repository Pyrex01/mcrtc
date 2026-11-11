// Controls painted the way Minecraft paints its menus, matching `src/ui/widgets.rs`. Sizes are in
// Minecraft GUI pixels, and [Gui.scale] screen pixels make one GUI pixel, a whole number like the
// game's GUI Scale, so the font and textures stay sharp.

package com.mcrtc.android.ui

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapShader
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.ColorFilter
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.Rect
import android.graphics.RectF
import android.graphics.Shader
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.text.InputType
import android.text.Layout
import android.text.StaticLayout
import android.text.TextPaint
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.EditText

const val FONT_SIZE = 9
const val BUTTON_HEIGHT = 20
const val COLUMN = 200
const val WIDE_COLUMN = 340
const val GAP = 4
const val HEADER = 33
const val FOOTER = 33
const val STRING_BOX = 54
private const val SLOT_HEIGHT = 36

const val WHITE = Color.WHITE
const val MUTED = 0xffa0a0a0.toInt()
const val YELLOW = 0xffffff55.toInt()
const val RED = 0xffff5555.toInt()
private const val GREEN = 0xff55ff55.toInt()
private const val FIELD_TEXT = 0xffe0e0e0.toInt()
private const val HINT = 0xff707070.toInt()
private const val TOOLTIP_FILL = 0xff100010.toInt()
private const val TOOLTIP_BORDER = 0xff28007f.toInt()
private const val SELECTION = 0xff0000aa.toInt()

/** Text shadows are a quarter of the text's brightness, one pixel down and right. */
fun shadowOf(color: Int): Int = Color.rgb(Color.red(color) / 4, Color.green(color) / 4, Color.blue(color) / 4)

class Gui(context: Context, val scale: Int) {
    val font: Typeface = font(context)

    fun px(gui: Int): Int = gui * scale

    fun paint(color: Int = WHITE, size: Int = FONT_SIZE) = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = font
        textSize = px(size).toFloat()
        this.color = color
    }

    /** How far below the top of its capitals a line's baseline sits. */
    fun capHeight(paint: Paint): Int {
        val bounds = Rect()
        paint.getTextBounds("H", 0, 1, bounds)
        return -bounds.top
    }

    /** Draws one line with its shadow. `capTop` is where the top of its capitals go. */
    fun text(canvas: Canvas, text: String, x: Float, capTop: Int, paint: Paint, color: Int, align: Paint.Align) {
        paint.textAlign = align
        val baseline = (capTop + capHeight(paint)).toFloat()
        paint.color = shadowOf(color)
        canvas.drawText(text, x + scale, baseline + scale, paint)
        paint.color = color
        canvas.drawText(text, x, baseline, paint)
    }

    companion object {
        private var cached: Typeface? = null

        fun font(context: Context): Typeface =
            cached ?: Typeface.createFromAsset(context.assets, "monocraft.ttf").also { cached = it }

        /** The largest whole scale that keeps a 320x240 screen in view, like GUI Scale: Auto. */
        fun scaleFor(width: Int, height: Int): Int = minOf(width / 320, height / 240).coerceAtLeast(1)
    }
}

/** A texture repeated across whatever it covers, `texel` screen pixels per texture pixel. */
class TileDrawable(bitmap: Bitmap, texel: Float) : Drawable() {
    private val paint = Paint().apply {
        isFilterBitmap = false
        shader = BitmapShader(bitmap, Shader.TileMode.REPEAT, Shader.TileMode.REPEAT).apply {
            setLocalMatrix(Matrix().apply { setScale(texel, texel) })
        }
    }

    override fun draw(canvas: Canvas) = canvas.drawRect(bounds, paint)
    override fun setAlpha(alpha: Int) = Unit
    override fun setColorFilter(filter: ColorFilter?) = Unit

    @Deprecated("Deprecated in Java")
    override fun getOpacity() = PixelFormat.OPAQUE
}

/** The earth behind every screen. A tile covers 32 GUI pixels, two per texel. */
fun background(gui: Gui) = TileDrawable(Art.earth, 2f * gui.scale)

/** The darker strip list screens keep between their title and their buttons. */
class BandDrawable(private val gui: Gui) : Drawable() {
    private val paint = Paint()

    override fun draw(canvas: Canvas) {
        val area = bounds
        val line = gui.scale.toFloat()
        fun fill(top: Float, bottom: Float, color: Int) {
            paint.color = color
            canvas.drawRect(area.left.toFloat(), top, area.right.toFloat(), bottom, paint)
        }
        fill(area.top.toFloat(), area.bottom.toFloat(), Color.argb(160, 0, 0, 0))
        fill(area.top.toFloat(), area.top + line, Color.argb(36, 255, 255, 255))
        fill(area.top + line, area.top + 2 * line, Color.BLACK)
        fill(area.bottom - 2 * line, area.bottom - line, Color.BLACK)
        fill(area.bottom - line, area.bottom.toFloat(), Color.argb(36, 255, 255, 255))
    }

    override fun setAlpha(alpha: Int) = Unit
    override fun setColorFilter(filter: ColorFilter?) = Unit

    @Deprecated("Deprecated in Java")
    override fun getOpacity() = PixelFormat.TRANSLUCENT
}

/** Draws the frame just inside a rectangle, the way the game outlines its controls. */
private fun outline(canvas: Canvas, gui: Gui, left: Float, top: Float, right: Float, bottom: Float, color: Int) {
    val paint = Paint().apply { this.color = color }
    val line = gui.scale.toFloat()
    canvas.drawRect(left, top, right, top + line, paint)
    canvas.drawRect(left, bottom - line, right, bottom, paint)
    canvas.drawRect(left, top, left + line, bottom, paint)
    canvas.drawRect(right - line, top, right, bottom, paint)
}

/** A stone button, outlined in white while pressed. */
@SuppressLint("ViewConstructor")
class McButton(context: Context, private val gui: Gui, label: String) : View(context) {
    var label: String = label
        set(value) {
            field = value
            contentDescription = value
            invalidate()
        }

    private val stone = TileDrawable(Art.stone, gui.scale.toFloat())
    private val paint = gui.paint()
    private val fill = Paint()

    init {
        contentDescription = label
        isClickable = true
        isFocusable = true
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        setMeasuredDimension(MeasureSpec.getSize(widthMeasureSpec), gui.px(BUTTON_HEIGHT))
    }

    override fun drawableStateChanged() {
        super.drawableStateChanged()
        invalidate()
    }

    override fun setEnabled(enabled: Boolean) {
        super.setEnabled(enabled)
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        val w = width.toFloat()
        val h = height.toFloat()
        val unit = gui.scale.toFloat()
        val lit = isEnabled && (isPressed || isFocused && !isInTouchMode)
        val color = if (isEnabled) WHITE else MUTED
        if (isEnabled) {
            stone.setBounds(0, 0, width, height)
            stone.draw(canvas)
            // Light along the top and left, shade along the bottom and right.
            fill.color = Color.argb(87, 255, 255, 255)
            canvas.drawRect(unit, unit, w - unit, 2 * unit, fill)
            canvas.drawRect(unit, unit, 2 * unit, h - unit, fill)
            fill.color = Color.argb(87, 0, 0, 0)
            canvas.drawRect(unit, h - 3 * unit, w - unit, h - unit, fill)
            canvas.drawRect(w - 2 * unit, unit, w - unit, h - unit, fill)
        } else {
            fill.color = Color.rgb(0x2c, 0x2c, 0x2c)
            canvas.drawRect(0f, 0f, w, h, fill)
        }
        outline(canvas, gui, 0f, 0f, w, h, if (lit) WHITE else Color.BLACK)

        val shown = TextUtils.ellipsize(label, paint, w - gui.px(8), TextUtils.TruncateAt.END).toString()
        val capTop = (height - gui.capHeight(paint)) / 2
        gui.text(canvas, shown, (w / 2f).let { Math.round(it).toFloat() }, capTop, paint, color, Paint.Align.CENTER)
    }
}

/** A wrapped block of text taking the full width it is given. */
@SuppressLint("ViewConstructor")
class McText(
    context: Context,
    private val gui: Gui,
    text: String,
    private val color: Int,
    private val center: Boolean = false,
    size: Int = FONT_SIZE,
) : View(context) {
    var text: String = text
        set(value) {
            if (field == value) return
            field = value
            contentDescription = value
            layout = null
            requestLayout()
            invalidate()
        }

    private val paint = gui.paint(color, size)
    private var layout: StaticLayout? = null

    init {
        contentDescription = text
    }

    private fun layoutFor(width: Int): StaticLayout {
        layout?.takeIf { it.width == width }?.let { return it }
        val alignment = if (center) Layout.Alignment.ALIGN_CENTER else Layout.Alignment.ALIGN_NORMAL
        return StaticLayout.Builder.obtain(text, 0, text.length, paint, width.coerceAtLeast(1))
            .setAlignment(alignment)
            .setIncludePad(false)
            .build()
            .also { layout = it }
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val width = MeasureSpec.getSize(widthMeasureSpec) - gui.scale
        setMeasuredDimension(width + gui.scale, layoutFor(width).height + gui.scale)
    }

    override fun onDraw(canvas: Canvas) {
        val layout = layoutFor(width - gui.scale)
        canvas.save()
        canvas.translate(gui.scale.toFloat(), gui.scale.toFloat())
        paint.color = shadowOf(color)
        layout.draw(canvas)
        canvas.restore()
        paint.color = color
        layout.draw(canvas)
    }
}

/** A title drawn centered in the band above a screen's body. */
@SuppressLint("ViewConstructor")
class Header(context: Context, private val gui: Gui, private val title: String) : View(context) {
    private val paint = gui.paint()

    init {
        contentDescription = title
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        setMeasuredDimension(MeasureSpec.getSize(widthMeasureSpec), gui.px(HEADER))
    }

    override fun onDraw(canvas: Canvas) {
        gui.text(canvas, title, width / 2f, gui.px(13), paint, WHITE, Paint.Align.CENTER)
    }
}

/** The black, outlined box of a text field, white edged while it has focus. */
private class FieldDrawable(private val gui: Gui) : Drawable() {
    private val paint = Paint()
    private var focused = false

    override fun isStateful() = true

    override fun onStateChange(state: IntArray): Boolean {
        val now = android.R.attr.state_focused in state
        if (now == focused) return false
        focused = now
        invalidateSelf()
        return true
    }

    override fun draw(canvas: Canvas) {
        val area = RectF(bounds)
        paint.color = Color.BLACK
        canvas.drawRect(area, paint)
        outline(canvas, gui, area.left, area.top, area.right, area.bottom, if (focused) WHITE else MUTED)
    }

    override fun setAlpha(alpha: Int) = Unit
    override fun setColorFilter(filter: ColorFilter?) = Unit

    @Deprecated("Deprecated in Java")
    override fun getOpacity() = PixelFormat.OPAQUE
}

/** A one-line text box, or a fixed-height scrolling one for the long strings when `lines`. */
@SuppressLint("ViewConstructor", "ClickableViewAccessibility")
class McField(
    context: Context,
    private val gui: Gui,
    hint: String,
    private val boxHeight: Int? = null,
) : EditText(context) {
    init {
        setTextSize(TypedValue.COMPLEX_UNIT_PX, gui.px(FONT_SIZE).toFloat())
        setTextColor(FIELD_TEXT)
        setHintTextColor(HINT)
        this.hint = hint
        highlightColor = SELECTION
        includeFontPadding = false
        setShadowLayer(0.01f, gui.scale.toFloat(), gui.scale.toFloat(), shadowOf(FIELD_TEXT))
        background = FieldDrawable(gui)
        if (Build.VERSION.SDK_INT >= 29) {
            textCursorDrawable = GradientDrawable().apply {
                setColor(FIELD_TEXT)
                setSize(gui.scale, 0)
            }
        }
        breakStrategy = Layout.BREAK_STRATEGY_SIMPLE
        hyphenationFrequency = Layout.HYPHENATION_FREQUENCY_NONE
        if (boxHeight == null) {
            val padding = gui.px(4)
            setPadding(padding, padding, padding, padding)
            isSingleLine = true
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            imeOptions = EditorInfo.IME_ACTION_DONE
        } else {
            val padding = gui.px(3)
            setPadding(padding, padding, padding, padding)
            gravity = Gravity.TOP or Gravity.START
            // The strings have no spaces and must survive untouched, so no autocorrect.
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
            isVerticalScrollBarEnabled = true
            // Scroll the box itself rather than the screen around it.
            setOnTouchListener { view, event ->
                if (event.action == MotionEvent.ACTION_DOWN && (canScrollVertically(1) || canScrollVertically(-1))) {
                    view.parent.requestDisallowInterceptTouchEvent(true)
                }
                false
            }
        }
        // After the input type, which picks a typeface of its own for password variations.
        typeface = gui.font
    }

    override fun setInputType(type: Int) {
        super.setInputType(type)
        // TextView's own constructor gets here before `gui` is set.
        @Suppress("SENSELESS_COMPARISON")
        if (gui != null) typeface = gui.font
    }

    /** Shows `text` for selecting and copying, without letting it be edited. */
    fun readOnly(text: String) {
        setText(text)
        keyListener = null
        setTextIsSelectable(true)
        typeface = gui.font
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val height = boxHeight?.let { MeasureSpec.makeMeasureSpec(gui.px(it), MeasureSpec.EXACTLY) }
        super.onMeasure(widthMeasureSpec, height ?: heightMeasureSpec)
    }
}

/** How many of the five ping bars are lit. */
private fun signal(ping: Long?): Int = when {
    ping == null -> 0
    ping < 80 -> 5
    ping < 150 -> 4
    ping < 300 -> 3
    ping < 600 -> 2
    else -> 1
}

/** One line of a list, drawn like an entry in the server list. */
@SuppressLint("ViewConstructor")
class ListEntry(
    context: Context,
    private val gui: Gui,
    private val name: String,
    private val detail: String,
    private val ping: Long?,
) : View(context) {
    private val paint = gui.paint()
    private val fill = Paint()

    init {
        contentDescription = "$name, $detail" + (ping?.let { ", $it ms" } ?: "")
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        setMeasuredDimension(MeasureSpec.getSize(widthMeasureSpec), gui.px(SLOT_HEIGHT))
    }

    override fun onDraw(canvas: Canvas) {
        val unit = gui.scale
        val face = Rect(2 * unit, 2 * unit, 34 * unit, 34 * unit)
        canvas.drawBitmap(Art.portrait(name), null, face, Paint().apply { isFilterBitmap = false })

        val left = (face.right + 3 * unit).toFloat()
        val textWidth = width - 2 * unit - left
        val top = 3 * unit
        val shownName = TextUtils.ellipsize(name, paint, textWidth - 20 * unit, TextUtils.TruncateAt.END)
        gui.text(canvas, shownName.toString(), left, top, paint, WHITE, Paint.Align.LEFT)
        val shownDetail = TextUtils.ellipsize(detail, paint, textWidth, TextUtils.TruncateAt.END)
        gui.text(canvas, shownDetail.toString(), left, top + 12 * unit, paint, MUTED, Paint.Align.LEFT)

        // Ping bars in the top right corner.
        val heights = intArrayOf(2, 3, 5, 6, 8)
        val lit = signal(ping)
        val color = when {
            lit >= 4 -> GREEN
            lit >= 2 -> YELLOW
            else -> RED
        }
        val right = width - 3 * unit
        val bottom = 11 * unit
        val barsLeft = right - 14 * unit
        for ((index, height) in heights.withIndex()) {
            val x = barsLeft + index * 3 * unit
            val bar = Rect(x, bottom - height * unit, x + 2 * unit, bottom)
            fill.color = Color.argb(128, 0, 0, 0)
            canvas.drawRect(Rect(bar).apply { offset(unit, unit) }, fill)
            fill.color = if (index < lit) color else Color.rgb(0x33, 0x33, 0x33)
            canvas.drawRect(bar, fill)
        }
    }
}

/** A notice that slides into the top right corner, drawn like a tooltip. */
class Toast(context: Context, private val gui: Gui) : View(context) {
    private var text = ""
    private val paint = gui.paint()
    private val fill = Paint()

    init {
        visibility = INVISIBLE
    }

    fun show(message: String) {
        text = message
        contentDescription = message
        requestLayout()
        visibility = VISIBLE
        announceForAccessibility(message)
        val hidden = (paint.measureText(text) + gui.px(14)).toFloat()
        animate().cancel()
        translationX = hidden
        animate().translationX(0f).setDuration(200).withEndAction {
            animate().translationX(hidden).setStartDelay(3_100).setDuration(200)
                .withEndAction { visibility = INVISIBLE }
        }
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        setMeasuredDimension(Math.round(paint.measureText(text)) + gui.px(10), gui.px(18))
    }

    override fun onDraw(canvas: Canvas) {
        fill.color = TOOLTIP_FILL
        canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), fill)
        val unit = gui.scale.toFloat()
        outline(canvas, gui, unit, unit, width - unit, height - unit, TOOLTIP_BORDER)
        gui.text(canvas, text, gui.px(5).toFloat(), (height - gui.capHeight(paint)) / 2, paint, WHITE, Paint.Align.LEFT)
    }
}
