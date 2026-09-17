package com.droidbridge.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import com.droidbridge.android.ui.DroidBridgeUi
import com.droidbridge.android.ui.state.AppViewModel

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val graph = (application as DroidBridgeApplication).requireAppGraph()
        val viewModel = ViewModelProvider(this, object : ViewModelProvider.Factory {
            @Suppress("UNCHECKED_CAST")
            override fun <T : ViewModel> create(modelClass: Class<T>): T =
                AppViewModel(graph.settings, graph.client) as T
        })[AppViewModel::class.java]
        setContent {
            DroidBridgeUi(viewModel, graph)
        }
    }
}
