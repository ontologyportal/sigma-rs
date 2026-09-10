import { createApp } from "vue";
import { createPinia } from "pinia";
import "../assets/styles.css";

import { router } from "./router";
import App from "./App.vue";

const app = createApp(App);
const pinia = createPinia();

// Pinia first: the router's guards call `useKBStore()`.
app.use(pinia).use(router).mount("#app");
