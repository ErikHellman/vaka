package se.hellsoft.vaka.sample

import android.app.Activity
import android.os.Bundle
import android.widget.Button
import android.widget.LinearLayout
import se.hellsoft.vaka.Vaka

class MainActivity : Activity() {
    private val cart = CartViewModel()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val layout = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        fun button(label: String, action: () -> Unit) =
            layout.addView(Button(this).apply { text = label; setOnClickListener { action() } })
        button("Track purchase") { Vaka.track("purchase", mapOf("plan" to "pro")) }
        button("Handled error") {
            try {
                cart.checkout()
            } catch (e: IllegalStateException) {
                Vaka.captureException(e)
            }
        }
        button("Crash") { cart.checkout() }
        button("Identify") { Vaka.identify("user-123") }
        setContentView(layout)
    }
}

class CartViewModel {
    private val items = mutableListOf<String>()

    fun checkout(): Int {
        Vaka.addBreadcrumb("cart", "checkout with ${items.size} items")
        check(items.isNotEmpty()) { "Cart is empty" }
        return items.size
    }
}
