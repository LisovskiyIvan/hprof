import { BrowserRouter, Navigate, Route, Routes } from "react-router"
import Home from "./pages/Home"
import Profile from "./pages/Profile"

function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route index element={<Home />} />
        <Route path="/profile/:id/*" element={<Profile />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </BrowserRouter>
  )
}

export default App
