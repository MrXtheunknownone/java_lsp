package dev.javalsp.testbed;

import com.google.gson.Gson;
import java.util.ArrayList;
import java.util.List;

public class GreetingLog {
    private final List<String> greetings = new ArrayList<>();

    public void record(String greeting) {
        greetings.add(greeting);
    }

    public String toJson() {
        return new Gson().toJson(greetings);
    }
}
