// The screens, drawn in the style of Minecraft's own menus like the desktop window in
// `src/ui/mod.rs`. The session itself lives in [Sessions], so it carries on while Minecraft is in
// front.

package com.mcrtc.android

import android.Manifest
import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.text.Editable
import android.text.InputFilter
import android.text.InputType
import android.text.TextWatcher
import android.view.Gravity
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowInsets
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import com.mcrtc.android.ui.BUTTON_HEIGHT
import com.mcrtc.android.ui.BandDrawable
import com.mcrtc.android.ui.COLUMN
import com.mcrtc.android.ui.FOOTER
import com.mcrtc.android.ui.GAP
import com.mcrtc.android.ui.Gui
import com.mcrtc.android.ui.Header
import com.mcrtc.android.ui.ListEntry
import com.mcrtc.android.ui.MUTED
import com.mcrtc.android.ui.McButton
import com.mcrtc.android.ui.McField
import com.mcrtc.android.ui.McText
import com.mcrtc.android.ui.RED
import com.mcrtc.android.ui.STRING_BOX
import com.mcrtc.android.ui.TitleArt
import com.mcrtc.android.ui.Toast
import com.mcrtc.android.ui.WHITE
import com.mcrtc.android.ui.WIDE_COLUMN
import com.mcrtc.android.ui.YELLOW
import com.mcrtc.android.ui.background
import com.mcrtc.android.ui.dots
import com.mcrtc.android.ui.randomSplash

private const val COPIED_MS = 1_800L

private fun plural(count: Int, word: String) = "$count $word${if (count == 1) "" else "s"}"

class MainActivity : Activity() {
    private val state get() = Sessions.state
    private lateinit var root: FrameLayout
    private lateinit var content: FrameLayout
    private var gui: Gui? = null
    private var toast: Toast? = null
    private var shown: Screen? = null
    /** Updates the screen in place, for screens whose fields must keep focus. */
    private var refresh: (() -> Unit)? = null
    private var splash = randomSplash()
    private var shownWidth = 0
    private var shownHeight = 0
    private val listener = { render() }

    override fun onCreate(saved: Bundle?) {
        super.onCreate(saved)
        Sessions.load(this)
        root = FrameLayout(this)
        content = FrameLayout(this)
        root.addView(content, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        setContentView(root)
        drawBehindSystemBars()

        root.setOnApplyWindowInsetsListener { _, insets ->
            val (bars, padding) = insetsOf(insets)
            content.setPadding(padding[0], padding[1], padding[2], padding[3])
            // The keyboard must not change the scale, or every field would lose its focus.
            val scale = Gui.scaleFor(root.width - bars[0] - bars[2], root.height - bars[1] - bars[3])
            if (root.width > 0 && scale != gui?.scale) {
                gui = Gui(this, scale)
                root.background = background(gui!!)
                shown = null
            }
            root.post { render() }
            insets
        }
        // Only on a real resize: rebuilding a screen lays it out again, which would loop.
        root.addOnLayoutChangeListener { _, left, top, right, bottom, oldLeft, oldTop, oldRight, oldBottom ->
            if (right - left != oldRight - oldLeft || bottom - top != oldBottom - oldTop) root.requestApplyInsets()
        }
    }

    @Suppress("DEPRECATION")
    private fun drawBehindSystemBars() {
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false)
        } else {
            window.decorView.systemUiVisibility = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
                View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
        }
        if (Build.VERSION.SDK_INT >= 29) window.isNavigationBarContrastEnforced = false
    }

    /** The system bars alone, then the bars and the keyboard together: left, top, right, bottom. */
    @Suppress("DEPRECATION")
    private fun insetsOf(insets: WindowInsets): Pair<IntArray, IntArray> {
        if (Build.VERSION.SDK_INT >= 30) {
            val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            val all = insets.getInsets(
                WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout() or WindowInsets.Type.ime()
            )
            return intArrayOf(bars.left, bars.top, bars.right, bars.bottom) to
                intArrayOf(all.left, all.top, all.right, all.bottom)
        }
        return intArrayOf(
            insets.stableInsetLeft, insets.stableInsetTop, insets.stableInsetRight, insets.stableInsetBottom,
        ) to intArrayOf(
            insets.systemWindowInsetLeft, insets.systemWindowInsetTop,
            insets.systemWindowInsetRight, insets.systemWindowInsetBottom,
        )
    }

    override fun onStart() {
        super.onStart()
        Sessions.listen(listener)
        render()
    }

    override fun onStop() {
        Sessions.unlisten(listener)
        Sessions.save(this)
        super.onStop()
    }

    // ---- Rendering ----

    private fun go(screen: Screen) {
        state.screen = screen
        render()
    }

    private fun render() {
        val gui = gui ?: return
        val width = content.width - content.paddingLeft - content.paddingRight
        val height = content.height - content.paddingTop - content.paddingBottom
        if (width <= 0 || height <= 0) return
        // Pages stretch with the keyboard; the centered screens are laid out for one size.
        val resized = (width != shownWidth || height != shownHeight) &&
            state.screen in setOf(Screen.Title, Screen.Progress, Screen.Failed)
        val update = refresh
        if (state.screen == shown && update != null && !resized) {
            update()
        } else {
            if (state.screen != shown && state.screen == Screen.Title) splash = randomSplash()
            shown = state.screen
            shownWidth = width
            shownHeight = height
            refresh = null
            content.removeAllViews()
            content.addView(build(gui, width / gui.scale, height / gui.scale))
            toast = Toast(this, gui).also {
                content.addView(
                    it,
                    FrameLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT, Gravity.TOP or Gravity.END).apply {
                        setMargins(0, gui.px(4), gui.px(4), 0)
                    },
                )
            }
        }
        state.toast?.let { message ->
            state.toast = null
            toast?.show(message)
        }
    }

    private fun build(gui: Gui, width: Int, height: Int): View = when (state.screen) {
        Screen.Title -> title(gui, width, height)
        Screen.HostSetup -> hostSetup(gui, width)
        Screen.Friends -> friends(gui, width)
        Screen.Name -> name(gui, width)
        Screen.Invite -> invite(gui, width)
        Screen.JoinSetup -> joinSetup(gui, width)
        Screen.Reply -> reply(gui, width)
        Screen.Joined -> joined(gui, width)
        Screen.Options -> options(gui, width)
        Screen.Progress -> progress(gui, width, height)
        Screen.Failed -> failed(gui, width, height)
    }

    // ---- Building blocks ----

    /** A vertical stack with the game's spacing between its items. */
    private inner class Column(val gui: Gui) {
        val layout = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }

        fun <T : View> add(view: T, spaced: Boolean = true, height: Int = WRAP_CONTENT): T {
            val params = LinearLayout.LayoutParams(MATCH_PARENT, height)
            if (spaced && layout.childCount > 0) params.topMargin = gui.px(GAP)
            layout.addView(view, params)
            return view
        }

        fun gap(height: Int) {
            add(View(this@MainActivity), spaced = false, height = gui.px(height))
        }

        fun text(text: String, color: Int, center: Boolean = false, size: Int = 9) =
            add(McText(this@MainActivity, gui, text, color, center, size))

        fun button(label: String, enabled: Boolean = true, action: () -> Unit) =
            add(McButton(this@MainActivity, gui, label).apply {
                isEnabled = enabled
                setOnClickListener { action() }
            })

        /** A captioned field whose text lives in the forms. */
        fun field(
            caption: String,
            hint: String,
            value: String,
            boxHeight: Int? = null,
            onChange: (String) -> Unit,
        ): McField {
            text(caption, MUTED)
            val field = McField(this@MainActivity, gui, hint, boxHeight).apply {
                setText(value)
                addTextChangedListener(object : TextWatcher {
                    override fun beforeTextChanged(text: CharSequence?, start: Int, count: Int, after: Int) = Unit
                    override fun onTextChanged(text: CharSequence?, start: Int, before: Int, count: Int) = Unit
                    override fun afterTextChanged(text: Editable?) = onChange(text.toString())
                })
            }
            return add(field)
        }

        /** Two columns side by side, for things that come in pairs. */
        fun halves(fill: (Column, Column) -> Unit) {
            val row = LinearLayout(this@MainActivity)
            val left = Column(gui)
            val right = Column(gui)
            fill(left, right)
            row.addView(left.layout, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            row.addView(
                right.layout,
                LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { marginStart = gui.px(GAP) },
            )
            add(row)
        }
    }

    /** A row of equally wide buttons, centered. */
    private fun buttons(gui: Gui, each: Int, vararg row: Triple<String, Boolean, () -> Unit>): LinearLayout {
        val layout = LinearLayout(this).apply { gravity = Gravity.CENTER_HORIZONTAL }
        row.forEachIndexed { index, (label, enabled, action) ->
            val button = McButton(this, gui, label).apply {
                isEnabled = enabled
                setOnClickListener { action() }
            }
            layout.addView(
                button,
                LinearLayout.LayoutParams(gui.px(each), gui.px(BUTTON_HEIGHT)).apply {
                    if (index > 0) marginStart = gui.px(GAP)
                },
            )
        }
        return layout
    }

    private fun button(label: String, action: () -> Unit) = Triple(label, true, action)

    /** The layout most screens share: a title, a scrolling column, and buttons along the bottom. */
    private fun page(
        gui: Gui,
        width: Int,
        title: String,
        column: Int,
        banded: Boolean,
        body: (Column) -> Unit,
        footer: (Int) -> LinearLayout,
    ): View {
        val columnWidth = minOf(column, width - 32)
        val page = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        page.addView(Header(this, gui, title), LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))

        val items = Column(gui)
        body(items)
        items.layout.setPadding(0, gui.px(GAP), 0, gui.px(GAP))
        val holder = FrameLayout(this)
        holder.addView(
            items.layout,
            FrameLayout.LayoutParams(gui.px(columnWidth), WRAP_CONTENT, Gravity.CENTER_HORIZONTAL),
        )
        val scroll = ScrollView(this).apply {
            isFillViewport = true
            isVerticalScrollBarEnabled = false
            overScrollMode = View.OVER_SCROLL_NEVER
            if (banded) background = BandDrawable(gui)
            addView(holder)
        }
        page.addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))

        val bottom = FrameLayout(this)
        bottom.addView(
            footer(columnWidth),
            FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = gui.px(FOOTER - 27) },
        )
        page.addView(bottom, LinearLayout.LayoutParams(MATCH_PARENT, gui.px(FOOTER)))
        return page
    }

    /** A column that starts `top` GUI pixels down, for the screens without a page. */
    private fun centered(gui: Gui, width: Int, top: Int, fill: (Column) -> Unit): View {
        val column = Column(gui)
        fill(column)
        val frame = FrameLayout(this)
        frame.addView(
            column.layout,
            FrameLayout.LayoutParams(gui.px(minOf(WIDE_COLUMN, width - 32)), WRAP_CONTENT, Gravity.CENTER_HORIZONTAL)
                .apply { topMargin = gui.px(top) },
        )
        return frame
    }

    private fun copy(button: McButton, label: String, text: String) {
        val clipboard = getSystemService(ClipboardManager::class.java)
        clipboard.setPrimaryClip(ClipData.newPlainText("mcrtc", text))
        button.label = "Copied!"
        button.postDelayed({ button.label = label }, COPIED_MS)
    }

    private fun share(text: String) {
        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
        startActivity(Intent.createChooser(send, null))
    }

    private fun pasteInto(field: McField) {
        val clipboard = getSystemService(ClipboardManager::class.java)
        val text = clipboard.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(this)
        if (!text.isNullOrBlank()) field.setText(cleanPaste(text.toString()))
    }

    /** Copy and Share side by side, for the strings that go to a friend. */
    private fun Column.sendButtons(gui: Gui, what: String, text: String) {
        halves { left, right ->
            val copyLabel = "Copy $what"
            left.button(copyLabel) {}.apply { setOnClickListener { copy(this, copyLabel, text) } }
            right.button("Share...") { share(text) }
        }
    }

    // ---- Screens ----

    private fun title(gui: Gui, width: Int, height: Int): View {
        val frame = FrameLayout(this)
        frame.addView(TitleArt(this, gui, splash, "mcrtc ${BuildConfig.VERSION_NAME}"))
        val column = Column(gui)
        column.button("Host a Server") { go(Screen.HostSetup) }
        column.button("Join a Friend") { go(Screen.JoinSetup) }
        column.add(
            buttons(
                gui, (COLUMN - GAP) / 2,
                button("Options...") { go(Screen.Options) },
                button("Quit") {
                    Sessions.stop()
                    finishAndRemoveTask()
                },
            )
        )
        frame.addView(
            column.layout,
            FrameLayout.LayoutParams(gui.px(minOf(COLUMN, width - 16)), WRAP_CONTENT, Gravity.CENTER_HORIZONTAL)
                // As on the desktop, but tall phones would leave a gulf under the logo.
                .apply { topMargin = gui.px(minOf(height / 4 + 48, 132)) },
        )
        refresh = {}
        return frame
    }

    private fun hostSetup(gui: Gui, width: Int): View {
        val forms = state.forms
        refresh = {}
        return page(gui, width, "Host a Server", COLUMN, false, { column ->
            column.field("Minecraft Server Address", DEFAULT_ADDRESS, forms.server) { forms.server = it }
                .apply { inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI }
            column.halves { left, right ->
                left.field("Max Friends", "No limit", forms.maxPeers) { forms.maxPeers = it }
                    .apply { inputType = InputType.TYPE_CLASS_NUMBER }
                right.field("Connections Each", "No limit", forms.maxConnections) { forms.maxConnections = it }
                    .apply { inputType = InputType.TYPE_CLASS_NUMBER }
            }
            column.gap(GAP)
            column.text(
                "Start your Java Edition server first, on this phone or on your network, then press " +
                    "Start Hosting.",
                MUTED,
            )
        }) { columnWidth ->
            buttons(
                gui, (minOf(COLUMN, columnWidth) - GAP) / 2,
                button("Start Hosting") { startHost() },
                button("Cancel") { go(Screen.Title) },
            )
        }
    }

    private fun friends(gui: Gui, width: Int): View {
        return page(gui, width, "Friends (${state.friends.size})", WIDE_COLUMN, true, { column ->
            state.waitingForSlot?.let { limit ->
                column.text("Full ($limit). Waiting for a friend to leave.", YELLOW, center = true)
            }
            if (state.friends.isEmpty()) {
                column.gap(56)
                column.text("No friends yet. Invite one!", MUTED, center = true)
            }
            for (friend in state.friends) {
                val connections = plural(friend.connections, "connection")
                val detail = describeRoute(friend.route)?.let { "$it · $connections" } ?: connections
                column.add(ListEntry(this, gui, friend.name, detail, friend.ping))
            }
        }) { columnWidth ->
            buttons(
                gui, minOf((columnWidth - GAP) / 2, 150),
                Triple("Invite a Friend", state.nameReady) {
                    state.forms.name = ""
                    go(Screen.Name)
                },
                button("Stop Hosting") { Sessions.stop() },
            )
        }
    }

    private fun name(gui: Gui, width: Int): View {
        val forms = state.forms
        refresh = {}
        lateinit var field: McField
        val view = page(gui, width, "Invite a Friend", COLUMN, false, { column ->
            field = column.field("Friend's Name", state.defaultName, forms.name) { forms.name = it }.apply {
                filters = arrayOf(InputFilter.LengthFilter(32))
                setOnEditorActionListener { _, action, _ ->
                    (action == EditorInfo.IME_ACTION_DONE).also { if (it) submitName() }
                }
            }
        }) { columnWidth ->
            buttons(
                gui, (minOf(COLUMN, columnWidth) - GAP) / 2,
                button("Create Invite") { submitName() },
                button("Cancel") { go(Screen.Friends) },
            )
        }
        field.post {
            field.requestFocus()
            getSystemService(InputMethodManager::class.java).showSoftInput(field, 0)
        }
        return view
    }

    private fun invite(gui: Gui, width: Int): View {
        val forms = state.forms
        lateinit var error: McText
        val view = page(gui, width, "Invite ${state.label}", WIDE_COLUMN, false, { column ->
            column.text("1. Send this invite (valid for 10 minutes)", MUTED)
            column.add(McField(this, gui, "", STRING_BOX).apply { readOnly(state.invite) })
            column.sendButtons(gui, "Invite", state.invite)
            column.text("Invites contain your IP address. Only share them with people you trust.", YELLOW)
            column.gap(GAP)
            val reply = column.field("2. Paste the reply from ${state.label}", "", forms.reply, STRING_BOX) {
                forms.reply = it
            }
            column.button("Paste Reply") { pasteInto(reply) }
            error = column.text(state.error, RED)
        }) { columnWidth ->
            buttons(
                gui, (minOf(COLUMN, columnWidth) - GAP) / 2,
                button("Connect") { submitReply() },
                button("Cancel") { Sessions.send("cancel") },
            )
        }
        refresh = { error.text = state.error }
        return view
    }

    private fun joinSetup(gui: Gui, width: Int): View {
        val forms = state.forms
        lateinit var error: McText
        val view = page(gui, width, "Join a Friend", WIDE_COLUMN, false, { column ->
            val invite = column.field("Paste the invite from the host", "", forms.invite, STRING_BOX * 3 / 2) {
                forms.invite = it
            }
            column.button("Paste Invite") { pasteInto(invite) }
            column.gap(GAP)
            column.field("Local Address for Minecraft", DEFAULT_ADDRESS, forms.listen) { forms.listen = it }
                .apply { inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI }
            error = column.text(state.error, RED)
        }) { columnWidth ->
            buttons(
                gui, (minOf(COLUMN, columnWidth) - GAP) / 2,
                button("Join Server") { startJoin() },
                button("Cancel") { cancelJoin() },
            )
        }
        refresh = { error.text = state.error }
        return view
    }

    private fun reply(gui: Gui, width: Int): View {
        refresh = {}
        return page(gui, width, "Send This Reply to the Host", WIDE_COLUMN, false, { column ->
            column.text("Your reply", MUTED)
            column.add(McField(this, gui, "", STRING_BOX * 3 / 2).apply { readOnly(state.reply) })
            column.sendButtons(gui, "Reply", state.reply)
            column.text("Replies contain your IP address. Only share them with people you trust.", YELLOW)
            column.gap(GAP * 2)
            column.text("Waiting for the host", WHITE, center = true)
            column.add(dots(this, gui))
        }) { columnWidth ->
            buttons(gui, minOf(COLUMN, columnWidth), button("Cancel") { Sessions.stop() })
        }
    }

    private fun joined(gui: Gui, width: Int): View {
        val joined = state.joined
        return page(gui, width, "Connected!", WIDE_COLUMN, false, { column ->
            if (joined == null) return@page
            column.text("Add this server in Minecraft", MUTED, center = true)
            column.gap(GAP)
            column.text(joined.address, WHITE, center = true, size = 18)
            column.gap(GAP)
            val label = "Copy Address"
            column.button(label) {}.apply { setOnClickListener { copy(this, label, joined.address) } }
            column.gap(GAP)
            column.add(ListEntry(this, gui, "Host", plural(joined.connections, "connection"), joined.ping))
            describeRoute(joined.route)?.let { column.text(it, MUTED, center = true) }
        }) { columnWidth ->
            buttons(gui, minOf(COLUMN, columnWidth), button("Disconnect") { Sessions.stop() })
        }
    }

    private fun options(gui: Gui, width: Int): View {
        val forms = state.forms
        refresh = {}
        return page(gui, width, "Options", COLUMN, false, { column ->
            column.field("STUN Servers (space separated)", "Public defaults", forms.stun) { forms.stun = it }
                .apply { inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI }
            column.field("TURN Server", "turn:example.com:3478", forms.turnUrl) { forms.turnUrl = it }
                .apply { inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI }
            column.halves { left, right ->
                left.field("TURN Username", "", forms.turnUser) { forms.turnUser = it }
                right.field("TURN Password", "", forms.turnPass) { forms.turnPass = it }.apply {
                    inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
                }
            }
            column.gap(GAP)
            column.text("TURN relays your traffic when a direct connection is impossible.", MUTED)
        }) { columnWidth ->
            buttons(gui, minOf(COLUMN, columnWidth), button("Done") { closeOptions() })
        }
    }

    private fun progress(gui: Gui, width: Int, height: Int): View =
        centered(gui, width, height / 2 - 30) { column ->
            column.text(state.progress.title, WHITE, center = true)
            column.gap(GAP)
            column.text(state.progress.detail, MUTED, center = true)
            column.gap(GAP)
            column.add(dots(this, gui))
        }

    private fun failed(gui: Gui, width: Int, height: Int): View {
        val back = state.backFromFailure()
        val label = if (back == Screen.Friends) "Back to Friends" else "Back to Title Screen"
        return centered(gui, width, height / 2 - 50) { column ->
            column.text(state.failure.title, MUTED, center = true)
            column.gap(GAP * 2)
            column.text(state.failure.detail, RED, center = true)
            column.gap(GAP * 4)
            column.button(label) { go(back) }
        }
    }

    // ---- Actions ----

    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        }
    }

    private fun startHost() {
        hideKeyboard()
        askForNotifications()
        // Before the call: the session's first events can arrive ahead of anything after it.
        state.showProgress("Starting", "One moment")
        Sessions.start(this, Mode.Host)?.let { state.fail("Could Not Start", it) }
        render()
    }

    private fun startJoin() {
        if (state.forms.invite.isBlank()) {
            state.error = "Paste the invite first."
            render()
            return
        }
        hideKeyboard()
        state.error = ""
        if (state.awaitingInvite) {
            Sessions.send(state.takeInvite())
            render()
            return
        }
        askForNotifications()
        state.beginJoin()
        Sessions.start(this, Mode.Join)?.let { state.fail("Could Not Start", it) }
        render()
    }

    private fun cancelJoin() {
        state.error = ""
        if (state.awaitingInvite) Sessions.stop() else state.screen = Screen.Title
        render()
    }

    private fun submitName() {
        hideKeyboard()
        state.nameReady = false
        val name = state.forms.name.trim()
        state.showProgress("Generating Invite", "Gathering network candidates")
        Sessions.send(name)
        render()
    }

    private fun submitReply() {
        val reply = cleanPaste(state.forms.reply)
        if (reply.isNotEmpty()) {
            hideKeyboard()
            state.error = ""
            Sessions.send(reply)
            render()
        }
    }

    private fun closeOptions() {
        hideKeyboard()
        Sessions.save(this)
        go(Screen.Title)
    }

    private fun hideKeyboard() {
        currentFocus?.let {
            getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(it.windowToken, 0)
            it.clearFocus()
        }
    }

    @Deprecated("Deprecated in Java")
    @Suppress("DEPRECATION")
    override fun onBackPressed() {
        when (state.screen) {
            Screen.HostSetup -> go(Screen.Title)
            Screen.Options -> closeOptions()
            Screen.Name -> go(Screen.Friends)
            Screen.JoinSetup -> cancelJoin()
            Screen.Invite -> Sessions.send("cancel")
            Screen.Failed -> go(state.backFromFailure())
            Screen.Progress -> Unit
            // A running session keeps going in the background.
            Screen.Friends, Screen.Reply, Screen.Joined -> moveTaskToBack(true)
            Screen.Title -> if (Sessions.isRunning) moveTaskToBack(true) else super.onBackPressed()
        }
    }
}
