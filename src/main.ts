import { createApp } from 'vue'
import { createPinia } from 'pinia'
import '@dsh-plugin/dsh-ui-components-vue/style.css'

import UiRoot from './UiRoot.vue'
import { installUi } from './ui'
import router from './router'
import { i18n } from './i18n'
import './styles/index.scss'

const app = createApp(UiRoot)

app.use(createPinia())
app.use(router)
app.use(i18n)
installUi(app)

app.mount('#app')
