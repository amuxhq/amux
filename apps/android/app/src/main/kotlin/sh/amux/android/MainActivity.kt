package sh.amux.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import sh.amux.design.AmuxTheme
import sh.amux.shell.ShellRoot

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        DebugHooks.onActivityCreated(this)
        setContent {
            AmuxTheme {
                ShellRoot()
            }
        }
    }
}
